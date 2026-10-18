This code has exactly one concurrency bug. Name the SHARED STATE at fault and the mechanism in one sentence each. Plain text, two sentences.

```rust
static mut HITS: u64 = 0;
fn handler() {
    unsafe { HITS += 1; }              // called from many tokio tasks
    let id = format!("req-{}", unsafe { HITS });
    log(&id);
}
```
