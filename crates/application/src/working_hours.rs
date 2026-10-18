//! Per-operator working hours & local-time delivery windows (CXA-F234).
//!
//! Operators are humans spread across timezones, but every timestamp in this
//! codebase is UTC ([`crate::state::now_rfc3339`]) and no tz concept existed
//! anywhere — so human-facing deliverables fired against UTC clocks that match
//! almost nobody's waking hours. This module models the ONE declaration each
//! operator makes ("Mon–Fri 09:00–17:30 at a fixed UTC offset", weekends off)
//! and answers the delivery questions as PURE functions of that declaration
//! and a UTC instant — no IO, no clock reads: the caller supplies the instant,
//! exactly like the `GitPort::working_tree`/`gates.rs` snapshot pattern.
//!
//! Model decisions (pinned by the CXA-F234 TDD contract):
//! - A declaration pins ONE fixed UTC offset (`tz_offset`, strict `±HH:MM`).
//!   IANA zone names are deliberately NOT modeled: `time` 0.3 ships no
//!   tz-database feature and the workspace carries no chrono-tz/tz-rs, so an
//!   IANA model means a NEW dependency — an SA decision, not an implementer's
//!   whim. [`resolve_windows`] keeps the door open: it arbitrates a boundary
//!   instant across CANDIDATE offsets, so a tz-database layer can slot in
//!   without changing the decision shape.
//! - The weekend/day rule IS the declaration: only weekdays with a declared
//!   window are actionable; a weekday with none (e.g. Saturday) is never
//!   actionable — weekends off by omission.
//! - Window containment is half-open `[start, end)` — the
//!   [`crate::config::in_quiet_window`] convention — so 09:00 is inside and
//!   17:30 is not: you stop working AT the end.
//! - `end <= start` is REJECTED at save, not silently accepted: a reversed or
//!   zero-length working window would make the operator unreachable forever
//!   with nothing on screen to say why. Wrap-midnight WORKING windows are not
//!   modeled (the AC never asks for one); the save error says so.
//!
//! Validation is fail-closed at the save seam (COX-B043 posture): the custom
//! serde impls below refuse an impossible offset or a malformed range while
//! the document is being deserialized, so [`crate::config_parse::parse_config`]
//! names the offending field and the Settings screen's whole-document
//! `PUT /api/projects/:pid/config` rejects the save. Nothing here defaults a
//! bad declaration into "no hours".

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{OffsetDateTime, Time, UtcOffset, Weekday};

// ---------------------------------------------------------------------------
// The declaration — one operator's working hours, as saved once via settings.
// ---------------------------------------------------------------------------

/// One operator's working hours (CXA-F234), keyed by bare username in
/// [`crate::config::HumanConfig::working_hours`]. The JSON shape is pinned by
/// the CXA-F234 TDD contract and survives a settings save round-trip
/// byte-stable:
///
/// ```json
/// { "tz_offset": "+02:00",
///   "windows": [ { "weekday": "monday", "start": "09:00", "end": "17:30" } ] }
/// ```
///
/// Every field is validated fail-closed at deserialize (see the serde impls
/// below): an impossible offset or a malformed range is a save error naming
/// the field, never a silently stored value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorWorkingHours {
    /// The operator's fixed local UTC offset, `±HH:MM` (mandatory sign,
    /// `+02:00` for Vienna in summer). One offset per declaration — the model
    /// decision in the module header.
    #[serde(with = "tz_serde")]
    pub tz_offset: UtcOffset,
    /// The declared windows, one per working day (a weekday may repeat for
    /// split shifts). A weekday absent here is not actionable — weekends off
    /// by omission.
    #[serde(default)]
    pub windows: Vec<WorkingWindow>,
}

/// One working window: a weekday and the local wall-clock range that is
/// actionable, half-open `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WorkingWindowRaw")]
pub struct WorkingWindow {
    /// Which local weekday the window sits on (lowercase full name in JSON —
    /// the spelling `time::Weekday` prints, lowercased).
    #[serde(with = "weekday_serde")]
    pub weekday: Weekday,
    /// Local start, strict `HH:MM` (00:00–23:59).
    #[serde(with = "hhmm_serde")]
    pub start: Time,
    /// Local end, strict `HH:MM`; must come after [`Self::start`] — enforced
    /// at save by [`WindowRangeError`].
    #[serde(with = "hhmm_serde")]
    pub end: Time,
}

impl WorkingWindow {
    /// Whether the local wall clock (`weekday`, minutes since local midnight)
    /// falls inside this window — half-open `[start, end)`, the
    /// [`crate::config::in_quiet_window`] convention.
    ///
    /// The save path guarantees `end > start` ([`WindowRangeError`]). The
    /// fields are plain data (every config section type here is), so a
    /// hand-built instance could violate that — such a window degrades to
    /// never covering anything, exactly as an empty range should.
    #[must_use]
    pub fn covers(&self, weekday: Weekday, minutes_of_day: u32) -> bool {
        self.weekday == weekday
            && (minutes_of(self.start)..minutes_of(self.end)).contains(&minutes_of_day)
    }
}

/// The deserialization carrier for [`WorkingWindow`] — identical fields, plus
/// the cross-field check [`TryFrom`] turns into a save error.
#[derive(Deserialize)]
struct WorkingWindowRaw {
    #[serde(with = "weekday_serde")]
    weekday: Weekday,
    #[serde(with = "hhmm_serde")]
    start: Time,
    #[serde(with = "hhmm_serde")]
    end: Time,
}

impl TryFrom<WorkingWindowRaw> for WorkingWindow {
    type Error = WindowRangeError;

    fn try_from(raw: WorkingWindowRaw) -> Result<Self, Self::Error> {
        if raw.end <= raw.start {
            return Err(WindowRangeError);
        }
        Ok(Self {
            weekday: raw.weekday,
            start: raw.start,
            end: raw.end,
        })
    }
}

/// A window whose end does not come after its start. Rejected at save — never
/// stored — because a reversed or zero-length working window would silently
/// make the operator unreachable forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "working window end must be after start — a reversed or empty range is not a working \
     window, and a wrap-midnight range is not supported; declare a within-day HH:MM-HH:MM \
     range (split shifts: one window per stretch)"
)]
pub struct WindowRangeError;

/// Minutes since midnight of a local wall-clock [`Time`].
fn minutes_of(t: Time) -> u32 {
    u32::from(t.hour()) * 60 + u32::from(t.minute())
}

// ---------------------------------------------------------------------------
// The decisions — pure functions over (declaration, instant [, candidates]).
// ---------------------------------------------------------------------------

/// "Is this UTC instant actionable for this operator?" (CXA-F234 AC2): true
/// only when the instant, read in the operator's declared local offset, falls
/// inside one of their declared windows on its declared weekday — and never
/// on a weekday they did not declare (the weekend/day rule). Pure over the
/// declaration and the instant; no clock, no IO.
///
/// An operator with no declaration is simply never actionable: callers gate
/// delivery on a declared window existing, so an undeclared operator's
/// delivery behaves exactly as before this feature.
#[must_use]
pub fn is_actionable(decl: &OperatorWorkingHours, now: OffsetDateTime) -> bool {
    !resolve_windows(decl, now, std::slice::from_ref(&decl.tz_offset)).is_empty()
}

/// Which of the operator's declared windows contain the UTC instant `now`,
/// read under each of `candidate_offsets` (CXA-F234 AC3).
///
/// A DST boundary instant is the one moment an operator's local wall clock is
/// genuinely ambiguous: the SAME UTC instant reads one hour apart across the
/// transition (2026-03-29 01:00 UTC is 02:00 local at +01:00 and 03:00 local
/// at +02:00). Callers that cannot know which side applies — e.g. a cached
/// offset from before the jump vs the fresh one — pass both candidates here.
/// The resolver then guarantees the AC's contract: the boundary instant falls
/// inside EXACTLY ONE correct resolved window — the same window matched via
/// two candidates is counted once, never twice, and a candidate that reads
/// outside every window contributes nothing.
///
/// Pure over (declaration, instant, candidates); returned in declaration
/// order, deduplicated by window content.
#[must_use]
pub fn resolve_windows<'a>(
    decl: &'a OperatorWorkingHours,
    now: OffsetDateTime,
    candidate_offsets: &[UtcOffset],
) -> Vec<&'a WorkingWindow> {
    let mut picked: Vec<&'a WorkingWindow> = Vec::new();
    for candidate in candidate_offsets {
        let local = now.to_offset(*candidate);
        let weekday = local.weekday();
        let minutes_of_day = u32::from(local.hour()) * 60 + u32::from(local.minute());
        for window in &decl.windows {
            if window.covers(weekday, minutes_of_day) && !picked.contains(&window) {
                picked.push(window);
            }
        }
    }
    picked
}

// ---------------------------------------------------------------------------
// Fail-closed serde: the save seam. Each helper refuses a value the model
// cannot represent, with a message naming what was wrong (COX-B043 posture),
// and serializes back the exact canonical form it accepts — so a declaration
// round-trips through a settings save unchanged.
// ---------------------------------------------------------------------------

/// `UtcOffset` as strict `±HH:MM` — the canonical form both sides agree on.
mod tz_serde {
    use super::{parse_tz, Deserializer, Serializer, UtcOffset};
    use serde::Deserialize;

    // serde's `serialize_with` pins the `&T` signature even for `Copy` types.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn serialize<S: Serializer>(
        tz: &UtcOffset,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let minutes = i32::from(tz.whole_minutes());
        let (sign, minutes) = if minutes < 0 {
            ('-', -minutes)
        } else {
            ('+', minutes)
        };
        serializer.collect_str(&format_args!(
            "{sign}{:02}:{:02}",
            minutes / 60,
            minutes % 60
        ))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<UtcOffset, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_tz(&text).map_err(serde::de::Error::custom)
    }
}

/// Strict `±HH:MM` parse for a declared local UTC offset: mandatory sign, two
/// ASCII digits each, hour 00–23 (no place on Earth is further off), minute
/// 00–59. `+99:00` and `+02:60` are refused, not clamped.
fn parse_tz(text: &str) -> Result<UtcOffset, String> {
    let invalid = || {
        format!("invalid tz_offset {text:?}: expected \"±HH:MM\" with a mandatory sign, e.g. \"+02:00\"")
    };
    let b = text.as_bytes();
    if b.len() != 6 || b[3] != b':' {
        return Err(invalid());
    }
    let sign: i32 = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return Err(invalid()),
    };
    let (Some(h), Some(m)) = (two_digits(&b[1..3]), two_digits(&b[4..6])) else {
        return Err(invalid());
    };
    if h > 23 || m > 59 {
        return Err(format!(
            "invalid tz_offset {text:?}: hour must be 00–23 and minute 00–59"
        ));
    }
    let (Ok(h), Ok(m)) = (
        i8::try_from(sign * i32::from(h)),
        i8::try_from(sign * i32::from(m)),
    ) else {
        return Err(invalid());
    };
    UtcOffset::from_hms(h, m, 0).map_err(|e| format!("invalid tz_offset {text:?}: {e}"))
}

/// Exactly two ASCII digits.
fn two_digits(s: &[u8]) -> Option<u8> {
    if s.len() == 2 && s.iter().all(u8::is_ascii_digit) {
        Some((s[0] - b'0') * 10 + (s[1] - b'0'))
    } else {
        None
    }
}

/// `time::Weekday` as the lowercase full name the CXA-F234 document contract
/// pins (`"monday"`, not `0` — `time`'s own serde emits a bare number).
mod weekday_serde {
    use super::{Deserializer, Serializer, Weekday};
    use serde::Deserialize;

    // serde's `serialize_with` pins the `&T` signature even for `Copy` types.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn serialize<S: Serializer>(
        weekday: &Weekday,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match weekday {
            Weekday::Monday => "monday",
            Weekday::Tuesday => "tuesday",
            Weekday::Wednesday => "wednesday",
            Weekday::Thursday => "thursday",
            Weekday::Friday => "friday",
            Weekday::Saturday => "saturday",
            Weekday::Sunday => "sunday",
        })
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Weekday, D::Error> {
        let text = String::deserialize(deserializer)?;
        match text.trim().to_ascii_lowercase().as_str() {
            "monday" => Ok(Weekday::Monday),
            "tuesday" => Ok(Weekday::Tuesday),
            "wednesday" => Ok(Weekday::Wednesday),
            "thursday" => Ok(Weekday::Thursday),
            "friday" => Ok(Weekday::Friday),
            "saturday" => Ok(Weekday::Saturday),
            "sunday" => Ok(Weekday::Sunday),
            _ => Err(serde::de::Error::custom(format!(
                "invalid weekday {text:?}: expected one of monday, tuesday, wednesday, \
                 thursday, friday, saturday, sunday"
            ))),
        }
    }
}

/// Local wall-clock time as strict `HH:MM` (00:00–23:59): `"25:00"`,
/// `"24:00"`, `"17:61"` and `"nine"` are refused, not clamped — a working
/// window must mean what it says.
mod hhmm_serde {
    use super::{parse_hhmm, Deserializer, Serializer, Time};
    use serde::Deserialize;

    // serde's `serialize_with` pins the `&T` signature even for `Copy` types.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn serialize<S: Serializer>(t: &Time, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{:02}:{:02}", t.hour(), t.minute()))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Time, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_hhmm(&text).map_err(serde::de::Error::custom)
    }
}

fn parse_hhmm(text: &str) -> Result<Time, String> {
    let invalid =
        || format!("invalid working-hours time {text:?}: expected \"HH:MM\" (00:00–23:59)");
    let Some((h, m)) = text.split_once(':') else {
        return Err(invalid());
    };
    if h.len() != 2 || m.len() != 2 {
        return Err(invalid());
    }
    let (Some(h), Some(m)) = (two_digits(h.as_bytes()), two_digits(m.as_bytes())) else {
        return Err(invalid());
    };
    if h >= 24 || m >= 60 {
        return Err(format!(
            "invalid working-hours time {text:?}: hour must be 00–23 and minute 00–59"
        ));
    }
    Time::from_hms(h, m, 0).map_err(|e| format!("invalid working-hours time {text:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    /// The document contract's own example operator: Vienna summer time,
    /// Mondays 09:00–17:30 local. Built through the serde path so every test
    /// exercises the save validation too.
    fn mira() -> OperatorWorkingHours {
        declaration("+02:00", "monday", "09:00", "17:30")
    }

    fn declaration(tz: &str, weekday: &str, start: &str, end: &str) -> OperatorWorkingHours {
        serde_json::from_value(json!({
            "tz_offset": tz,
            "windows": [{ "weekday": weekday, "start": start, "end": end }]
        }))
        .expect("a valid declaration loads")
    }

    fn declaration_with_windows(
        tz: UtcOffset,
        windows: Vec<WorkingWindow>,
    ) -> OperatorWorkingHours {
        OperatorWorkingHours {
            tz_offset: tz,
            windows,
        }
    }

    fn local_weekday_and_minutes(
        decl: &OperatorWorkingHours,
        now: OffsetDateTime,
    ) -> (Weekday, u32) {
        let local = now.to_offset(decl.tz_offset);
        (
            local.weekday(),
            u32::from(local.hour()) * 60 + u32::from(local.minute()),
        )
    }

    // --- AC2: the actionable truth table over real timestamps --------------

    #[test]
    fn inside_a_declared_window_on_a_declared_weekday_is_actionable() {
        // 2026-08-24 07:30 UTC = Monday 09:30 local in Vienna (+02:00).
        let decl = mira();
        let now = datetime!(2026-08-24 07:30 UTC);
        assert_eq!(
            local_weekday_and_minutes(&decl, now),
            (Weekday::Monday, 9 * 60 + 30),
            "the fixture is what it claims: a Monday mid-window local instant"
        );
        assert!(is_actionable(&decl, now));
    }

    #[test]
    fn the_same_local_time_on_an_undeclared_weekday_is_not_actionable() {
        // Same 09:30 wall clock, next day: Tuesday is not declared — the
        // weekday rule excludes it even though the time is identical.
        assert!(!is_actionable(&mira(), datetime!(2026-08-25 07:30 UTC)));
    }

    #[test]
    fn a_weekend_instant_is_not_actionable() {
        // Sunday 18:30 local in Vienna — weekends off by omission.
        assert!(!is_actionable(&mira(), datetime!(2026-08-30 16:30 UTC)));
    }

    #[test]
    fn outside_the_window_is_not_actionable_and_the_edges_follow_half_open() {
        // 07:30 local — before the window.
        assert!(!is_actionable(&mira(), datetime!(2026-08-24 05:30 UTC)));
        // 09:00 local exactly — the window includes its start.
        assert!(is_actionable(&mira(), datetime!(2026-08-24 07:00 UTC)));
        // 17:30 local exactly — the window excludes its end (in_quiet_window
        // convention: you stop working AT the end).
        assert!(!is_actionable(&mira(), datetime!(2026-08-24 15:30 UTC)));
        // 17:29 local — still inside.
        assert!(is_actionable(&mira(), datetime!(2026-08-24 15:29 UTC)));
    }

    // --- AC3: DST boundary instants resolve to exactly one window ----------

    #[test]
    fn a_dst_boundary_instant_resolves_to_exactly_one_window() {
        // European spring-forward: 2026-03-29 01:00 UTC (a Sunday), CET
        // +01:00 → CEST +02:00. Under BOTH sides of the transition the instant
        // reads inside the same Sunday 02:00–04:00 window (02:00 before the
        // jump, 03:00 after it) — yet it must resolve to that window ONCE.
        let spring = datetime!(2026-03-29 01:00 UTC);
        let winter = UtcOffset::from_hms(1, 0, 0).expect("+01:00 is a legal offset");
        let summer = UtcOffset::from_hms(2, 0, 0).expect("+02:00 is a legal offset");
        let decl = declaration_with_windows(
            winter,
            vec![serde_json::from_value(json!({
                "weekday": "sunday", "start": "02:00", "end": "04:00"
            }))
            .expect("valid window")],
        );
        let window = &decl.windows[0];
        for candidate in [winter, summer] {
            let (weekday, minutes) = {
                let local = spring.to_offset(candidate);
                (
                    local.weekday(),
                    u32::from(local.hour()) * 60 + u32::from(local.minute()),
                )
            };
            assert_eq!(weekday, Weekday::Sunday);
            assert!(
                window.covers(weekday, minutes),
                "the boundary instant reads inside the window under {candidate}"
            );
        }
        let resolved = resolve_windows(&decl, spring, &[winter, summer]);
        assert_eq!(
            resolved.len(),
            1,
            "a DST boundary instant falls inside exactly one correct resolved window"
        );
        assert_eq!(
            (resolved[0].start.hour(), resolved[0].start.minute()),
            (2, 0)
        );
    }

    #[test]
    fn a_fall_back_boundary_instant_also_resolves_to_exactly_one_window() {
        // European fall-back: 2026-10-25 01:00 UTC (a Sunday), CEST +02:00 →
        // CET +01:00. The instant reads 03:00 local before the jump and 02:00
        // after — the same window once.
        let fall = datetime!(2026-10-25 01:00 UTC);
        let summer = UtcOffset::from_hms(2, 0, 0).expect("+02:00 is a legal offset");
        let winter = UtcOffset::from_hms(1, 0, 0).expect("+01:00 is a legal offset");
        let decl = declaration_with_windows(
            winter,
            vec![serde_json::from_value(json!({
                "weekday": "sunday", "start": "02:00", "end": "04:00"
            }))
            .expect("valid window")],
        );
        assert_eq!(resolve_windows(&decl, fall, &[summer, winter]).len(), 1);
    }

    #[test]
    fn a_candidate_that_reads_outside_every_window_contributes_nothing() {
        // The mirror of the boundary contract: only ONE side of the transition
        // reads inside (02:00 at +01:00 is in 01:30–02:30; 03:00 at +02:00 is
        // not) — the resolver must still land on exactly that one window.
        let spring = datetime!(2026-03-29 01:00 UTC);
        let winter = UtcOffset::from_hms(1, 0, 0).expect("+01:00 is a legal offset");
        let summer = UtcOffset::from_hms(2, 0, 0).expect("+02:00 is a legal offset");
        let decl = declaration_with_windows(
            winter,
            vec![serde_json::from_value(json!({
                "weekday": "sunday", "start": "01:30", "end": "02:30"
            }))
            .expect("valid window")],
        );
        let resolved = resolve_windows(&decl, spring, &[winter, summer]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            (resolved[0].start.hour(), resolved[0].start.minute()),
            (1, 30)
        );
    }

    #[test]
    fn an_ordinary_instant_resolves_its_one_window_under_the_declared_offset() {
        let decl = mira();
        let now = datetime!(2026-08-24 07:30 UTC); // Monday 09:30 local
        let resolved = resolve_windows(&decl, now, &[decl.tz_offset]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].weekday, Weekday::Monday);
        // Outside every window: nothing resolves.
        assert!(
            resolve_windows(&decl, datetime!(2026-08-24 15:30 UTC), &[decl.tz_offset]).is_empty()
        );
    }

    // --- The save seam: fail-closed validation -----------------------------

    #[test]
    fn an_impossible_or_ambiguous_tz_offset_is_refused() {
        for tz in ["+99:00", "+02:60", "02:00", "+2:00", "UTC", "+24:00"] {
            assert!(
                serde_json::from_value::<OperatorWorkingHours>(json!({
                    "tz_offset": tz,
                    "windows": [{ "weekday": "monday", "start": "09:00", "end": "17:30" }]
                }))
                .is_err(),
                "{tz} must be refused at save"
            );
        }
    }

    #[test]
    fn a_missing_tz_offset_is_refused_not_defaulted_to_utc() {
        // The offset is the declaration's load-bearing axis — an absent one
        // must be a save error, never a silent "assume UTC".
        assert!(serde_json::from_value::<OperatorWorkingHours>(json!({
            "windows": [{ "weekday": "monday", "start": "09:00", "end": "17:30" }]
        }))
        .is_err());
    }

    #[test]
    fn a_declaration_without_windows_defaults_to_never_actionable() {
        // `windows` may be omitted (#[serde(default)]): the operator declared
        // an offset but no working day — nothing is actionable, exactly as if
        // every weekday had been left undeclared.
        let decl: OperatorWorkingHours = serde_json::from_value(json!({ "tz_offset": "+02:00" }))
            .expect("an offset with no windows is a valid, empty declaration");
        assert!(decl.windows.is_empty());
        assert!(!is_actionable(&decl, datetime!(2026-08-24 07:30 UTC)));
    }

    #[test]
    fn a_malformed_window_range_is_refused() {
        for (start, end) in [
            ("25:00", "17:30"), // hour out of range
            ("09:00", "24:00"), // end hour out of range
            ("09:00", "17:61"), // minute out of range
            ("nine", "17:30"),  // not an HH:MM range at all
            ("9:00", "17:30"),  // not two digits
        ] {
            assert!(
                serde_json::from_value::<OperatorWorkingHours>(json!({
                    "tz_offset": "+02:00",
                    "windows": [{ "weekday": "monday", "start": start, "end": end }]
                }))
                .is_err(),
                "the range {start}-{end} must be refused at save"
            );
        }
    }

    #[test]
    fn a_reversed_or_zero_length_range_is_refused_not_silently_never() {
        for (start, end) in [("17:30", "09:00"), ("09:00", "09:00")] {
            let err = serde_json::from_value::<OperatorWorkingHours>(json!({
                "tz_offset": "+02:00",
                "windows": [{ "weekday": "monday", "start": start, "end": end }]
            }))
            .expect_err("a window that is never open must not load as if it were");
            assert!(
                err.to_string().contains("end must be after start"),
                "the refusal must say why, got: {err}"
            );
        }
    }

    #[test]
    fn an_unknown_weekday_name_is_refused() {
        assert!(serde_json::from_value::<OperatorWorkingHours>(json!({
            "tz_offset": "+02:00",
            "windows": [{ "weekday": "mondays", "start": "09:00", "end": "17:30" }]
        }))
        .is_err());
    }

    #[test]
    fn the_declaration_round_trips_byte_stable_through_a_settings_save() {
        let json = json!({
            "tz_offset": "-05:30",
            "windows": [
                { "weekday": "monday", "start": "09:00", "end": "17:30" },
                { "weekday": "friday", "start": "08:15", "end": "12:45" }
            ]
        });
        let decl: OperatorWorkingHours =
            serde_json::from_value(json.clone()).expect("a valid declaration loads");
        assert_eq!(
            serde_json::to_value(&decl).expect("the declaration serializes"),
            json,
            "the canonical form the save writes back must equal what was saved"
        );
        // And the round-tripped declaration still decides correctly.
        assert!(is_actionable(
            &decl,
            // Friday 08:15 local — the second window's inclusive start.
            datetime!(2026-08-28 13:45 UTC)
        ));
    }
}
