//! Salvaging a `coxagent.json` that one malformed field would otherwise wipe.
//!
//! `serde_json::from_str::<Config>` is all-or-nothing: a single field it cannot
//! read — a port that does not fit `u16`, a string where a number belongs, a
//! typo'd enum — fails the whole document, and every caller that answers a
//! parse failure with `Config::default()` thereby throws away every OTHER
//! setting in the file. That is how `deploy.auto_rollback: true` silently
//! becomes `false` because a neighbouring port had one digit too many
//! (COX-B050).
//!
//! [`salvage_config`] narrows the blast radius to the field that is actually
//! broken: it deserializes, and on failure defaults exactly the field serde
//! points at, then tries again — recording each one. What comes back is the
//! operator's config with holes patched, plus the list of holes, so the app
//! keeps their settings AND can say what it could not read.
//!
//! Pure: it takes text and returns values. Deciding what to do about the
//! defects (warn, refuse to heal, show the operator) belongs to the caller.

use serde_json::Value;

use crate::config::Config;

/// One field the config file could not supply, and therefore was defaulted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfigDefect {
    /// Dotted path of the field, e.g. `deploy.host_port`.
    pub path: String,
    /// Why it could not be used, in the deserializer's own words.
    pub reason: String,
}

impl std::fmt::Display for ConfigDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} — using the default", self.path, self.reason)
    }
}

/// A config as the process can actually use it, plus every field that had to
/// be defaulted to get there.
#[derive(Debug, Clone)]
pub struct SalvagedConfig {
    /// The operator's settings, with only the unreadable fields defaulted.
    pub config: Config,
    /// Empty when the file was read exactly as written.
    pub defects: Vec<ConfigDefect>,
}

impl SalvagedConfig {
    /// Whether the file was usable exactly as written.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.defects.is_empty()
    }
}

/// How many fields we are willing to patch before concluding the document is
/// not a config at all. Config has fewer than a hundred leaves; a document
/// needing more repairs than this is noise, not a typo.
const REPAIR_BUDGET: usize = 64;

/// Read a `Config` out of `text`, defaulting ONLY the fields that are
/// malformed and keeping every field that is not.
///
/// # Errors
/// The text is not JSON at all (truncated write, half-flushed file), or the
/// document is so far from a config that field-by-field repair cannot rescue
/// it. Either way the caller gets to decide, rather than being handed silent
/// defaults.
pub fn salvage_config(text: &str) -> Result<SalvagedConfig, String> {
    let mut value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let defaults = serde_json::to_value(Config::default())
        .map_err(|e| format!("config defaults are not representable as JSON: {e}"))?;
    let mut defects: Vec<ConfigDefect> = Vec::new();
    for _ in 0..REPAIR_BUDGET {
        let err = match serde_path_to_error::deserialize::<_, Config>(value.clone()) {
            Ok(config) => return Ok(SalvagedConfig { config, defects }),
            Err(err) => err,
        };
        let steps = addressable_steps(err.path());
        let reason = err.into_inner().to_string();
        let Some(path) = patch(&mut value, &steps, &defaults) else {
            return Err(format!("{}: {reason}", display(&steps)));
        };
        defects.push(ConfigDefect { path, reason });
    }
    Err(format!(
        "config is malformed in more than {REPAIR_BUDGET} places — refusing to guess at it"
    ))
}

/// A location inside the JSON document that we can address to patch it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Field(String),
    Index(usize),
}

/// The deepest prefix of serde's error path that names a real container slot.
///
/// `Enum`/`Unknown` segments are positions inside a value, not slots in the
/// document, so they stop the walk: the enclosing field is the smallest thing
/// we can honestly replace.
fn addressable_steps(path: &serde_path_to_error::Path) -> Vec<Step> {
    let mut steps = Vec::new();
    for segment in path {
        match segment {
            serde_path_to_error::Segment::Seq { index } => steps.push(Step::Index(*index)),
            serde_path_to_error::Segment::Map { key } => steps.push(Step::Field(key.clone())),
            serde_path_to_error::Segment::Enum { .. } | serde_path_to_error::Segment::Unknown => {
                break
            }
        }
    }
    steps
}

fn render(steps: &[Step]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for step in steps {
        match step {
            Step::Field(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Step::Index(i) => {
                let _ = write!(out, "[{i}]");
            }
        }
    }
    out
}

fn at<'a>(value: &'a Value, steps: &[Step]) -> Option<&'a Value> {
    let mut cur = value;
    for step in steps {
        cur = match step {
            Step::Field(key) => cur.get(key)?,
            Step::Index(i) => cur.get(i)?,
        };
    }
    Some(cur)
}

fn at_mut<'a>(value: &'a mut Value, steps: &[Step]) -> Option<&'a mut Value> {
    let mut cur = value;
    for step in steps {
        cur = match step {
            Step::Field(key) => cur.get_mut(key)?,
            Step::Index(i) => cur.get_mut(i)?,
        };
    }
    Some(cur)
}

/// Make the slot serde tripped on readable again, and name what was changed.
/// `None` means nothing could be changed — the caller must stop rather than
/// loop, and report the document as unsalvageable.
///
/// Two shapes of breakage, in the order they are worth trying:
/// 1. An object is missing fields serde required — fill them in from the
///    defaults, which keeps every field the operator DID write.
/// 2. A slot holds a value serde cannot read — replace just that slot with its
///    default, or drop it when the defaults have nothing to put there (an
///    element of a list, an entry of a map).
///
/// The document ROOT is deliberately not replaceable: swapping the whole thing
/// for defaults is the very wipe this module exists to prevent, so a root that
/// is not an object at all is an error, not a repair.
fn patch(value: &mut Value, steps: &[Step], defaults: &Value) -> Option<String> {
    if fill_missing_fields(value, steps, defaults) {
        return Some(display(steps));
    }
    if steps.is_empty() {
        return None;
    }
    match (at(defaults, steps), at(value, steps)) {
        // The slot exists in a default config: put the default there.
        (Some(default), Some(current)) if default != current => {
            let default = default.clone();
            let slot = at_mut(value, steps)?;
            *slot = default;
            Some(display(steps))
        }
        // A field serde wanted that is simply absent here — insert its default.
        (Some(default), None) => {
            let default = default.clone();
            insert(value, steps, default).then(|| display(steps))
        }
        // Nothing in the defaults corresponds to it (a list element, a map
        // entry keyed by something the operator invented): it can only go.
        _ => remove(value, steps).then(|| display(steps)),
    }
}

/// Add the fields a default config has at `steps` and this document does not.
/// This is what rescues `"engine": {}` — an object missing a required field —
/// without discarding the fields that ARE there.
///
/// Optional neighbours get filled alongside the required one; that is a no-op
/// by construction (they are written with the value they would have defaulted
/// to), which is why this reports the object, not a field per key: the field
/// that was actually required is named by serde's own message.
fn fill_missing_fields(value: &mut Value, steps: &[Step], defaults: &Value) -> bool {
    let Some(Value::Object(default_fields)) = at(defaults, steps) else {
        return false;
    };
    let missing: Vec<(String, Value)> = {
        let Some(Value::Object(current)) = at(value, steps) else {
            return false;
        };
        default_fields
            .iter()
            .filter(|(key, _)| !current.contains_key(*key))
            .map(|(key, default)| (key.clone(), default.clone()))
            .collect()
    };
    if missing.is_empty() {
        return false;
    }
    let Some(Value::Object(current)) = at_mut(value, steps) else {
        return false;
    };
    for (key, default) in missing {
        current.insert(key, default);
    }
    true
}

/// How a path is shown to an operator; the whole document is "config".
fn display(steps: &[Step]) -> String {
    if steps.is_empty() {
        "config".to_owned()
    } else {
        render(steps)
    }
}

/// Split a path into its parent and the slot it names; the root has no parent.
fn split(steps: &[Step]) -> Option<(&[Step], &Step)> {
    steps.split_last().map(|(last, parent)| (parent, last))
}

fn insert(value: &mut Value, steps: &[Step], new: Value) -> bool {
    let Some((parent_steps, slot)) = split(steps) else {
        return false;
    };
    match (at_mut(value, parent_steps), slot) {
        (Some(Value::Object(parent)), Step::Field(key)) => {
            parent.insert(key.clone(), new);
            true
        }
        _ => false,
    }
}

fn remove(value: &mut Value, steps: &[Step]) -> bool {
    let Some((parent_steps, slot)) = split(steps) else {
        return false;
    };
    match (at_mut(value, parent_steps), slot) {
        (Some(Value::Object(parent)), Step::Field(key)) => parent.remove(key).is_some(),
        (Some(Value::Array(parent)), Step::Index(i)) if *i < parent.len() => {
            parent.remove(*i);
            true
        }
        _ => false,
    }
}

/// A config document built from the real defaults with `patch` applied on top,
/// so tests state only the field under test.
#[cfg(test)]
fn document(patch: &Value) -> String {
    let mut doc = serde_json::to_value(Config::default()).expect("defaults as json");
    merge(&mut doc, patch);
    serde_json::to_string(&doc).expect("document as text")
}

#[cfg(test)]
fn merge(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(into), Value::Object(from)) => {
            for (key, value) in from {
                merge(into.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (into, from) => *into = from.clone(),
    }
}

#[cfg(test)]
#[allow(clippy::needless_pass_by_value)] // json! macro reads better inline
fn salvage_document(patch: Value) -> SalvagedConfig {
    salvage_config(&document(&patch)).expect("a config document must salvage")
}

/// Like [`document`], but `section` is REPLACED rather than merged — the only
/// way to express "this section does not mention that field at all".
#[cfg(test)]
fn salvage_replacing(section: &str, value: &Value) -> SalvagedConfig {
    let mut doc = serde_json::to_value(Config::default()).expect("defaults as json");
    doc[section] = value.clone();
    salvage_config(&serde_json::to_string(&doc).expect("document as text"))
        .expect("a config document must salvage")
}

#[cfg(test)]
mod tests {
    use super::{salvage_config, salvage_document, salvage_replacing, Config, ConfigDefect};
    use serde_json::json;

    /// AC (COX-B050), the ticket's own repro: a `host_port` that does not fit
    /// `u16` must cost the operator that ONE field, not the neighbouring
    /// safety settings. `auto_rollback` flipping itself off because a port had
    /// one digit too many is the bug.
    #[test]
    fn an_out_of_range_port_does_not_take_its_neighbours_with_it() {
        let salvaged = salvage_document(json!({
            "deploy": {
                "host_port": 99_999,
                "enabled": true,
                "auto_rollback": true,
                "max_rollback_age_secs": 42,
            }
        }));

        assert_eq!(salvaged.config.deploy.host_port, None, "the bad field goes");
        assert!(salvaged.config.deploy.auto_rollback, "and only the bad one");
        assert_eq!(salvaged.config.deploy.max_rollback_age_secs, 42);
        assert!(salvaged.config.deploy.enabled);
        assert_eq!(
            salvaged
                .defects
                .iter()
                .map(|d| d.path.clone())
                .collect::<Vec<_>>(),
            vec!["deploy.host_port".to_owned()],
            "and the loss is reported, not silent"
        );
    }

    /// The same must hold for the other ways a port goes wrong — a string
    /// where a number belongs is the commonest hand-edit slip of all.
    #[test]
    fn a_string_port_costs_only_the_port() {
        let salvaged = salvage_document(json!({
            "deploy": { "host_port": "8101", "auto_rollback": true }
        }));

        assert_eq!(salvaged.config.deploy.host_port, None);
        assert!(salvaged.config.deploy.auto_rollback);
        assert_eq!(salvaged.defects.len(), 1);
    }

    /// A negative port cannot be a `u16` either (COX-B035's fixture value).
    #[test]
    fn a_negative_port_costs_only_the_port() {
        let salvaged = salvage_document(json!({
            "deploy": { "host_port": -1, "auto_rollback": true }
        }));

        assert_eq!(salvaged.config.deploy.host_port, None);
        assert!(salvaged.config.deploy.auto_rollback);
        assert_eq!(
            salvaged.defects.first().map(|d| d.path.as_str()),
            Some("deploy.host_port")
        );
    }

    /// `null` is a legal `Option<u16>` and an absent field is legal too:
    /// neither is a defect, and neither may be reported as one — an operator
    /// who never set a port must not be told their config is broken.
    #[test]
    fn a_null_or_missing_port_is_not_a_defect() {
        let explicit_null = salvage_document(json!({ "deploy": { "host_port": null } }));
        assert!(explicit_null.is_clean(), "null is a legal Option<u16>");
        assert_eq!(explicit_null.config.deploy.host_port, None);

        // A `deploy` section that never mentions `host_port` at all.
        let absent = salvage_replacing("deploy", &json!({ "auto_rollback": true }));
        assert!(absent.is_clean(), "an unset port is not a defect");
        assert_eq!(absent.config.deploy.host_port, None);
        assert!(absent.config.deploy.auto_rollback);
    }

    /// A file with nothing wrong with it round-trips untouched — repair must
    /// never be a licence to rewrite working configs.
    #[test]
    fn a_valid_config_is_returned_exactly_as_written() {
        let salvaged = salvage_document(json!({
            "deploy": { "host_port": 8101, "auto_rollback": true },
            "workflow": { "sleep_seconds": 7 },
        }));

        assert!(salvaged.is_clean());
        assert_eq!(salvaged.config.deploy.host_port, Some(8101));
        assert_eq!(salvaged.config.workflow.sleep_seconds, 7);
    }

    /// Breakage in one section leaves the others alone: sections are not
    /// all-or-nothing any more than the document is.
    #[test]
    fn a_bad_field_in_one_section_leaves_the_others_intact() {
        let salvaged = salvage_document(json!({
            "workflow": { "sleep_seconds": "soon" },
            "deploy": { "host_port": 8101, "auto_rollback": true },
        }));

        assert_eq!(salvaged.config.deploy.host_port, Some(8101));
        assert!(salvaged.config.deploy.auto_rollback);
        assert_eq!(
            salvaged.defects.first().map(|d| d.path.as_str()),
            Some("workflow.sleep_seconds")
        );
    }

    /// Several broken fields are each defaulted and each reported — the loop
    /// keeps going instead of stopping at the first one.
    #[test]
    fn every_broken_field_is_reported_and_the_rest_survive() {
        let salvaged = salvage_document(json!({
            "deploy": { "host_port": 99_999, "auto_rollback": true, "enabled": "yes" },
            "workflow": { "sleep_seconds": "soon", "sandbox": true },
        }));

        assert!(salvaged.config.deploy.auto_rollback);
        assert!(salvaged.config.workflow.sandbox);
        let mut paths: Vec<&str> = salvaged.defects.iter().map(|d| d.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "deploy.enabled",
                "deploy.host_port",
                "workflow.sleep_seconds"
            ]
        );
    }

    /// A required field the operator never wrote is filled from the defaults
    /// rather than failing the document — `"engine": {}` is a real thing to
    /// find in a hand-written file, and the sections around it are innocent.
    #[test]
    fn a_missing_required_field_is_filled_and_the_rest_kept() {
        let mut doc = serde_json::to_value(Config::default()).expect("defaults as json");
        doc["engine"] = json!({});
        doc["deploy"] = json!({ "host_port": 8101, "auto_rollback": true });

        let salvaged =
            salvage_config(&serde_json::to_string(&doc).expect("text")).expect("must salvage");

        assert_eq!(salvaged.config.deploy.host_port, Some(8101));
        assert!(salvaged.config.deploy.auto_rollback);
        let defect = salvaged.defects.first().expect("the gap is reported");
        assert_eq!(defect.path, "engine");
        assert!(
            defect.reason.contains("missing field `default`"),
            "the field that was actually required is named: {}",
            defect.reason
        );
    }

    /// A malformed element of a list is dropped, not the list — one bad
    /// architecture rule must not disable conformance checking wholesale.
    #[test]
    fn a_bad_list_element_is_dropped_and_the_good_ones_stay() {
        let rule = json!({ "area": "backend", "language": "Rust", "require_any": ["Cargo.toml"] });
        let salvaged = salvage_document(json!({ "architecture": [rule, 7] }));

        assert_eq!(salvaged.config.architecture.len(), 1, "the good rule stays");
        assert_eq!(salvaged.defects.len(), 1, "the bad element is reported");
    }

    /// Text that is not JSON is not a config with a bad field in it — the
    /// caller is told so instead of being handed plausible-looking defaults.
    #[test]
    fn text_that_is_not_json_is_an_error_not_a_default_config() {
        assert!(salvage_config("{\"deploy\": ").is_err());
        assert!(salvage_config("not json at all").is_err());
    }

    /// A JSON document that is not an object at all cannot be repaired
    /// field-by-field; saying so beats patching a number into a config.
    #[test]
    fn a_json_document_that_is_not_a_config_is_an_error() {
        assert!(salvage_config("[1, 2, 3]").is_err());
        assert!(salvage_config("42").is_err());
    }

    /// The defect reads as an explanation, not a struct dump — it is shown to
    /// an operator.
    #[test]
    fn a_defect_says_what_was_lost_and_what_replaced_it() {
        let defect = ConfigDefect {
            path: "deploy.host_port".to_owned(),
            reason: "invalid value: integer `99999`".to_owned(),
        };

        assert_eq!(
            defect.to_string(),
            "deploy.host_port: invalid value: integer `99999` — using the default"
        );
    }
}
