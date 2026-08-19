// Binary crate stub so the task has two files; the bug is NOT necessarily here.
mod store;
fn main() {
    let s = store::Store { entries: std::collections::HashMap::new() };
    let _ = s.get("boot", 0);
}
