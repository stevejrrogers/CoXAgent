A Rust service crashes at startup with:

    thread 'main' panicked at 'called `Result::unwrap()` on an `Err` value: Os { code: 98, kind: AddrInUse, message: "Address already in use" }', src/server.rs:42

In ONE sentence, state the root cause (what condition in the environment makes bind fail), then in ONE sentence the correct fix direction. Plain text.
