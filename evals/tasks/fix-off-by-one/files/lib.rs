/// Returns whether `idx` is a valid index into a slice of length `len`.
pub fn in_bounds(idx: usize, len: usize) -> bool {
    idx <= len
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn last_valid_index_is_len_minus_one() {
        assert!(in_bounds(0, 1));
        assert!(!in_bounds(1, 1));
        assert!(!in_bounds(5, 3));
    }
}
