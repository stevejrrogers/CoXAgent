//! Structured metrics registry for operational observability (CXA-C039).
//!
//! # Layering
//! This is a PURE application-layer data structure: atomic registers and a
//! mutex over an in-memory map, no IO of any kind (the hexagonal ratchet
//! applies). The decision of *what* to record is made by callers; rendering
//! ([`encode_prometheus`]) is pure string building; transport (the admin
//! listener, the telemetry middleware) lives in the presentation adapters.
//!
//! # Shape
//! Bounded-cardinality series keyed by `(name, fixed label map)`:
//!
//! * counters ([`MetricsRegistry::inc_counter`]) — rendered with the
//!   Prometheus `_total` suffix convention,
//! * latency histograms ([`MetricsRegistry::observe_duration`]) — fixed
//!   cumulative buckets ([`BUCKETS`]) plus `_sum`/`_count`,
//! * scalar gauges ([`MetricsRegistry::set_gauge`]).
//!
//! Cardinality is CAPPED at [`MAX_SERIES`] per kind: an unbounded label
//! dimension (say, a raw `:pid` or an attacker-probed path) would otherwise
//! grow the process without limit. Series beyond the cap are dropped and
//! counted in `cxa_metrics_series_dropped_total` so the cap itself is
//! observable. Dynamic label values must be pre-bucketed with
//! [`label_bucket`] before they reach the registry.
//!
//! Records are process-lifetime registers held in memory only — ephemeral
//! hit/duration/uptime stats are lost on restart by design; nothing is
//! persisted.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Histogram bucket boundaries in seconds (CXA-C039 contract: [0.005..10s]).
/// Rendered cumulative; the implicit final `+Inf` bucket equals `_count`.
pub const BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Upper bound on distinct series (per kind). Generous for a hub's route ×
/// method × status-class matrix, small enough that a label bug cannot OOM the
/// process.
pub const MAX_SERIES: usize = 512;

/// Content type the Prometheus text exposition format v0.0.4 scrapers expect.
pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

/// Counter base name for HTTP requests (rendered with a `_total` suffix).
pub const HTTP_REQUESTS: &str = "cxa_http_request";
/// Histogram name for HTTP request duration, in seconds.
pub const HTTP_REQUEST_DURATION: &str = "cxa_http_request_duration_seconds";
/// Gauge name for process uptime, in seconds (derived from registry start).
pub const PROCESS_UPTIME: &str = "cxa_process_uptime_seconds";
/// Counter for series dropped because the cardinality cap was hit.
pub const SERIES_DROPPED: &str = "cxa_metrics_series_dropped";

/// Ordered, fixed label set identifying one time series.
type Labels = Vec<(String, String)>;

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SeriesKey {
    name: String,
    labels: Labels,
}

/// One monotone counter. `fetch_add` outside the registry mutex keeps
/// concurrent increments lock-free (ordering relaxed: exposure reads need no
/// intra-series ordering guarantees beyond atomicity).
#[derive(Default)]
struct MetricCounter {
    value: AtomicU64,
}

/// One latency histogram: cumulative per-bucket counts, plus sum/count.
/// `observe` increments every bucket whose boundary is ≥ the sample, so the
/// stored counts are already cumulative as Prometheus expects.
struct MetricHistogram {
    buckets: Vec<AtomicU64>,
    sum_bits: AtomicU64,
    count: AtomicU64,
}

impl Default for MetricHistogram {
    fn default() -> Self {
        Self {
            buckets: BUCKETS.iter().map(|_| AtomicU64::new(0)).collect(),
            sum_bits: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

/// Process-lifetime registry of operational metrics.
///
/// Thread-safe; share via `Arc`. All mutation is interior — callers hold
/// `&self`, so one `Arc<MetricsRegistry>` can serve both the telemetry
/// middleware and the metrics admin listener.
#[derive(Default)]
pub struct MetricsRegistry {
    started: Option<Instant>,
    counters: Mutex<HashMap<SeriesKey, Arc<MetricCounter>>>,
    histograms: Mutex<HashMap<SeriesKey, Arc<MetricHistogram>>>,
    gauges: Mutex<BTreeMap<String, f64>>,
    dropped: AtomicU64,
}

impl MetricsRegistry {
    /// Create a registry whose uptime clock starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Some(Instant::now()),
            ..Self::default()
        }
    }

    /// Increment the counter identified by `(name, labels)` — creating the
    /// series on first use, or silently dropping the increment (and counting
    /// the drop) once [`MAX_SERIES`] distinct series exist.
    pub fn inc_counter(&self, name: &str, labels: &[(&str, &str)]) {
        let key = SeriesKey {
            name: name.to_owned(),
            labels: canonical_labels(labels),
        };
        if let Some(counter) = get_or_admit(&self.counters, key, &self.dropped) {
            counter.value.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record one latency sample (seconds) into the histogram identified by
    /// `(name, labels)`, updating every cumulative bucket it falls in plus
    /// `_sum`/`_count`. Samples beyond the last [`BUCKETS`] boundary still
    /// count toward `_count`/`_sum` (the implicit `+Inf` bucket).
    pub fn observe_duration(&self, name: &str, labels: &[(&str, &str)], seconds: f64) {
        let key = SeriesKey {
            name: name.to_owned(),
            labels: canonical_labels(labels),
        };
        if let Some(hist) = get_or_admit(&self.histograms, key, &self.dropped) {
            // One ordered pass: every boundary >= the sample is cumulative.
            for (i, bound) in BUCKETS.iter().enumerate() {
                if seconds <= *bound {
                    hist.buckets[i].fetch_add(1, Ordering::Relaxed);
                }
            }
            hist.sum_bits.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |bits| Some((f64::from_bits(bits) + seconds).to_bits()),
            ).ok();
            hist.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Set a scalar gauge, replacing any previous value.
    ///
    /// # Panics
    /// Panics if the gauges mutex is poisoned (another thread panicked while
    /// holding it) — a state this process cannot recover from meaningfully.
    pub fn set_gauge(&self, name: &str, value: f64) {
        #[allow(clippy::expect_used)]
        self.gauges
            .lock()
            .expect("MetricsRegistry gauges mutex should never be poisoned")
            .insert(name.to_owned(), value);
    }

    /// Seconds since this registry was created (process uptime).
    #[must_use]
    pub fn uptime_seconds(&self) -> u64 {
        self.started.map_or(0, |t| t.elapsed().as_secs())
    }

    /// Number of live distinct series (counters + histograms) — for tests and
    /// capacity assertions on the bounded-cardinality logic.
    #[must_use]
    pub fn series_count(&self) -> usize {
        let counters = self
            .counters
            .lock()
            .map_or(0, |m| m.len());
        let histograms = self
            .histograms
            .lock()
            .map_or(0, |m| m.len());
        counters + histograms
    }

    /// Counters dropped because [`MAX_SERIES`] was reached.
    #[must_use]
    pub fn dropped_series(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Canonical label vector: sorted by key so one series has exactly one key
/// regardless of the order the caller happened to list labels in.
fn canonical_labels(labels: &[(&str, &str)]) -> Labels {
    let mut out: Labels = labels
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    out.sort();
    out
}

/// Get-or-create a series under the cap: the map lock guards admission only;
/// the returned `Arc` is incremented lock-free afterwards.
fn get_or_admit<T: Default>(
    map: &Mutex<HashMap<SeriesKey, Arc<T>>>,
    key: SeriesKey,
    dropped: &AtomicU64,
) -> Option<Arc<T>> {
    #[allow(clippy::expect_used)]
    let mut guard = map
        .lock()
        .expect("MetricsRegistry series mutex should never be poisoned");
    if guard.len() >= MAX_SERIES && !guard.contains_key(&key) {
        dropped.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some(Arc::clone(guard.entry(key).or_default()))
}

/// Collapse a concrete, potentially unbounded request path into a bounded
/// family label for series that did not match a route pattern.
///
/// Project-scoped paths collapse onto the `:pid` family (any dynamic id maps
/// to the same key); every other path keeps at most its first two segments.
/// This is what keeps a scanner probing `/api/projects/x/y/z/…` from minting
/// unbounded series: N input paths produce O(1) family keys.
#[must_use]
pub fn label_bucket(raw: &str) -> String {
    let segments: Vec<&str> = raw.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [] => "/".to_owned(),
        ["api", "projects", _pid] => "/api/projects/:pid".to_owned(),
        ["api", "projects", _pid, ..] => "/api/projects/:pid/*".to_owned(),
        [one] => format!("/{one}"),
        [first, second, ..] => format!("/{first}/{second}"),
    }
}

/// Render the registry in the Prometheus text exposition format v0.0.4.
///
/// Pure string building over the current register values: counters with the
/// `_total` suffix, histograms as cumulative `_bucket{le=…}` lines plus
/// `_sum`/`_count`, gauges verbatim, then process uptime. Series are sorted
/// so the output is byte-stable across scrapes.
#[must_use]
pub fn encode_prometheus(registry: &MetricsRegistry) -> String {
    let mut out = String::new();

    if let Ok(counters) = registry.counters.lock() {
        let mut keys: Vec<&SeriesKey> = counters.keys().collect();
        keys.sort();
        for key in keys {
            let value = counters[key].value.load(Ordering::Relaxed);
            let full = format!("{}_total", key.name);
            push_help_type(&mut out, &full, "counter", &key.name);
            push_sample(&mut out, &full, &key.labels, &value.to_string());
        }
    }
    if let Ok(histos) = registry.histograms.lock() {
        let mut keys: Vec<&SeriesKey> = histos.keys().collect();
        keys.sort();
        for key in keys {
            let hist = &histos[key];
            let count = hist.count.load(Ordering::Relaxed);
            let sum = f64::from_bits(hist.sum_bits.load(Ordering::Relaxed));
            push_help_type(&mut out, &key.name, "histogram", &key.name);
            // Stored counts are already cumulative (observe increments every
            // boundary ≥ the sample); render them directly.
            for (i, bound) in BUCKETS.iter().enumerate() {
                let cumulative = hist.buckets[i].load(Ordering::Relaxed);
                let mut labels = key.labels.clone();
                labels.push(("le".to_owned(), format_le(*bound)));
                push_sample(
                    &mut out,
                    &format!("{}_bucket", key.name),
                    &labels,
                    &cumulative.to_string(),
                );
            }
            let mut inf = key.labels.clone();
            inf.push(("le".to_owned(), "+Inf".to_owned()));
            push_sample(
                &mut out,
                &format!("{}_bucket", key.name),
                &inf,
                &count.to_string(),
            );
            push_sample(
                &mut out,
                &format!("{}_sum", key.name),
                &key.labels,
                &format!("{sum}"),
            );
            push_sample(
                &mut out,
                &format!("{}_count", key.name),
                &key.labels,
                &count.to_string(),
            );
        }
    }
    if let Ok(gauges) = registry.gauges.lock() {
        for (name, value) in gauges.iter() {
            push_help_type(&mut out, name, "gauge", name);
            push_sample(&mut out, name, &[], &format!("{value}"));
        }
    }

    let dropped_total = registry.dropped.load(Ordering::Relaxed);
    push_help_type(
        &mut out,
        &format!("{SERIES_DROPPED}_total"),
        "counter",
        SERIES_DROPPED,
    );
    push_sample(
        &mut out,
        &format!("{SERIES_DROPPED}_total"),
        &[],
        &dropped_total.to_string(),
    );

    push_help_type(&mut out, PROCESS_UPTIME, "gauge", PROCESS_UPTIME);
    push_sample(
        &mut out,
        PROCESS_UPTIME,
        &[],
        &registry.uptime_seconds().to_string(),
    );
    out
}

/// Format a bucket boundary the way Prometheus scrapers expect: no exponent,
/// integral values keep one decimal.
fn format_le(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

fn push_help_type(out: &mut String, metric: &str, kind: &str, help_subject: &str) {
    // Writes to a String cannot fail; `let _` is the idiomatic discard.
    let _ = writeln!(out, "# HELP {metric} {help_subject}.");
    let _ = writeln!(out, "# TYPE {metric} {kind}");
}

fn push_sample(out: &mut String, name: &str, labels: &[(String, String)], value: &str) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (i, (k, v)) in labels.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "{k}=\"{}\"", escape_label(v));
        }
        out.push('}');
    }
    out.push(' ');
    out.push_str(value);
    out.push('\n');
}

/// Escape a label value per the exposition format (`\`, `"` and newline).
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse exposition output into (metric_name, labels, value) triples.
    fn parse_lines(text: &str) -> Vec<(String, BTreeMap<String, String>, String)> {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| {
                let (head, value) = l.rsplit_once(' ').expect("sample has a value");
                let (name, label_map) = match (head.find('{'), head.ends_with('}')) {
                    (Some(brace), true) => {
                        let inner = &head[brace + 1..head.len() - 1];
                        let mut map = BTreeMap::new();
                        for pair in inner.split("\",") {
                            let (k, v) = pair.split_once("=\"").expect("label pair");
                            map.insert(k.to_owned(), v.trim_end_matches('"').to_owned());
                        }
                        (head[..brace].to_owned(), map)
                    }
                    _ => (head.to_owned(), BTreeMap::new()),
                };
                (name, label_map, value.to_owned())
            })
            .collect()
    }

    #[test]
    fn inc_counter_renders_total_suffix_with_labels() {
        let registry = MetricsRegistry::new();
        registry.inc_counter(HTTP_REQUESTS, &[("route", "/api/health"), ("method", "GET")]);
        registry.inc_counter(HTTP_REQUESTS, &[("route", "/api/health"), ("method", "GET")]);

        let text = encode_prometheus(&registry);
        let samples = parse_lines(&text);
        let hits: Vec<_> = samples
            .iter()
            .filter(|(n, _, _)| n == "cxa_http_request_total")
            .collect();
        assert_eq!(hits.len(), 1, "one series, rendered once: {text}");
        assert_eq!(hits[0].1["route"], "/api/health");
        assert_eq!(hits[0].1["method"], "GET");
        assert_eq!(hits[0].2, "2", "two increments land in the one series");
        assert!(text.contains("# TYPE cxa_http_request_total counter"));
    }

    #[test]
    fn observe_duration_places_sample_in_correct_bucket_and_updates_sum_count() {
        let registry = MetricsRegistry::new();
        let labels: &[(&str, &str)] = &[("route", "/api/health")];
        registry.observe_duration(HTTP_REQUEST_DURATION, labels, 0.004);
        registry.observe_duration(HTTP_REQUEST_DURATION, labels, 0.02);
        registry.observe_duration(HTTP_REQUEST_DURATION, labels, 30.0);

        let text = encode_prometheus(&registry);
        let samples = parse_lines(&text);
        let bucket = |le: &str| {
            samples
                .iter()
                .find(|(n, l, _)| n == "cxa_http_request_duration_seconds_bucket" && l["le"] == le)
                .map_or_else(
                    || panic!("bucket le={le} missing in {text}"),
                    |(_, _, v)| v.as_str(),
                )
        };
        // Cumulative: 0.005 holds the 0.004 sample; 0.01 adds nothing; 0.025
        // adds the 0.02 sample; 30.0 lands only in +Inf.
        assert_eq!(bucket("0.005"), "1");
        assert_eq!(bucket("0.01"), "1");
        assert_eq!(bucket("0.025"), "2");
        assert_eq!(bucket("10.0"), "2");
        assert_eq!(bucket("+Inf"), "3");
        let sum: Vec<_> = samples
            .iter()
            .filter(|(n, _, _)| n == "cxa_http_request_duration_seconds_sum")
            .collect();
        assert_eq!(sum.len(), 1);
        let sum_val: f64 = sum[0].2.parse().expect("sum parses as f64");
        assert!((sum_val - 30.024).abs() < 1e-9, "sum = {sum_val}");
        let count: Vec<_> = samples
            .iter()
            .filter(|(n, _, _)| n == "cxa_http_request_duration_seconds_count")
            .collect();
        assert_eq!(count[0].2, "3");
    }

    #[test]
    fn distinct_label_sets_do_not_collide() {
        let registry = MetricsRegistry::new();
        registry.inc_counter(HTTP_REQUESTS, &[("route", "/a"), ("status", "2xx")]);
        registry.inc_counter(HTTP_REQUESTS, &[("route", "/a"), ("status", "5xx")]);
        registry.inc_counter(HTTP_REQUESTS, &[("route", "/b"), ("status", "2xx")]);

        let text = encode_prometheus(&registry);
        let samples: Vec<_> = parse_lines(&text)
            .into_iter()
            .filter(|(n, _, _)| n == "cxa_http_request_total")
            .collect();
        assert_eq!(samples.len(), 3, "three distinct label sets, three series");
        assert_eq!(registry.series_count(), 3);
    }

    #[test]
    fn concurrent_increments_never_lose_a_count() {
        let registry = Arc::new(MetricsRegistry::new());
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let registry = Arc::clone(&registry);
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        if t % 2 == 0 {
                            registry.inc_counter(HTTP_REQUESTS, &[("route", "/hot")]);
                        } else {
                            registry.inc_counter(HTTP_REQUESTS, &[("route", "/warm")]);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("test threads do not panic");
        }

        let text = encode_prometheus(&registry);
        let samples: Vec<_> = parse_lines(&text)
            .into_iter()
            .filter(|(n, _, _)| n == "cxa_http_request_total")
            .collect();
        assert_eq!(samples.len(), 2);
        let total: u64 = samples.iter().map(|(_, _, v)| v.parse::<u64>().expect("count"))
            .sum();
        assert_eq!(total, 8_000, "atomic increments preserve every count");
    }

    #[test]
    fn cardinality_is_capped_and_drops_are_counted() {
        let registry = MetricsRegistry::new();
        for i in 0..(MAX_SERIES + 100) {
            registry.inc_counter(HTTP_REQUESTS, &[("route", &format!("/runaway/{i}"))]);
        }
        assert_eq!(registry.series_count(), MAX_SERIES);
        assert_eq!(registry.dropped_series(), 100);
        let text = encode_prometheus(&registry);
        assert!(
            text.contains("cxa_metrics_series_dropped_total 100"),
            "the cap must be observable: {text}"
        );
    }

    #[test]
    fn label_bucket_collapses_dynamic_pids_to_one_family_key() {
        // The property: N distinct dynamic paths produce O(1) family keys, so
        // series count equals distinct bucketed keys regardless of volume.
        let registry = MetricsRegistry::new();
        for i in 0..10_000 {
            let route = label_bucket(&format!("/api/projects/p{i}/chat"));
            registry.inc_counter(HTTP_REQUESTS, &[("route", &route)]);
        }
        assert_eq!(label_bucket("/api/projects/anything"), "/api/projects/:pid");
        assert_eq!(label_bucket("/api/projects/anything/chat"), "/api/projects/:pid/*");
        assert_eq!(label_bucket("/healthz"), "/healthz");
        assert_eq!(label_bucket("/"), "/");
        assert_eq!(registry.series_count(), 1, "10k inputs, one family series");
    }

    #[test]
    fn label_values_are_escaped_per_exposition_format() {
        let registry = MetricsRegistry::new();
        registry.set_gauge("cxa_test_gauge", 1.5);
        registry.inc_counter(HTTP_REQUESTS, &[("route", "/we\"ird\\path")]);
        let text = encode_prometheus(&registry);
        assert!(text.contains("route=\"/we\\\"ird\\\\path\""), "{text}");
    }

    #[test]
    fn uptime_and_empty_registry_render_without_panicking() {
        let registry = MetricsRegistry::new();
        let text = encode_prometheus(&registry);
        assert!(text.contains("cxa_process_uptime_seconds 0"), "{text}");
    }

    #[test]
    fn gauge_set_replaces_previous_value() {
        let registry = MetricsRegistry::new();
        registry.set_gauge("cxa_active_hubs", 3.0);
        registry.set_gauge("cxa_active_hubs", 7.0);
        let text = encode_prometheus(&registry);
        assert!(text.contains("cxa_active_hubs 7"), "{text}");
        assert!(!text.contains("cxa_active_hubs 3"), "{text}");
    }
}
