//! CXA-B211 negative-proof fixture (deleted immediately after the proof):
//! a fabricated regression in the application layer doing direct IO.
pub fn cxa_b211_deliberate_violation() -> usize {
    std::fs::read_dir(".")
        .map(|entries| entries.count())
        .unwrap_or(0)
}
