/// Key-value store contract: `get` returns the value ONLY if it has not
/// expired at `now` (expiry timestamps are exclusive: an entry whose
/// `expires_at == now` is already dead).
pub struct Entry { pub value: String, pub expires_at: u64 }
pub struct Store { pub entries: std::collections::HashMap<String, Entry> }
impl Store {
    pub fn get(&self, key: &str, now: u64) -> Option<&str> {
        let e = self.entries.get(key)?;
        if is_live(e.expires_at, now) { Some(&e.value) } else { None }
    }
}
pub fn is_live(expires_at: u64, now: u64) -> bool { expires_at >= now }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entry_expiring_now_is_dead() {
        let mut m = std::collections::HashMap::new();
        m.insert("k".into(), Entry { value: "v".into(), expires_at: 10 });
        let s = Store { entries: m };
        assert_eq!(s.get("k", 9), Some("v"));
        assert_eq!(s.get("k", 10), None);
        assert_eq!(s.get("k", 11), None);
    }
}
