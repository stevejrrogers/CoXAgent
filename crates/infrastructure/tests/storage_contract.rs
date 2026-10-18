//! Contract for the S3 object-store presence probe (`StoragePort::exists`).
//!
//! CXA-B122 review: `S3Storage::exists` feeds the forensics view's artifact
//! check, where `exists == true` is treated as "evidence present". The probe
//! must therefore answer `false` — never panic, never propagate — when the
//! object store is unreachable or the key is unsafe. The transport-error
//! contract is exactly the semantic the CXA-B122 clippy fix preserved
//! (`map(..).unwrap_or(false)` → `is_ok_and(..)` at the `signed(..).await`
//! site); these tests pin it so any future refactor that changes the
//! error-path answer fails here instead of in review.
//!
//! No server and no bound port: the unreachable endpoint is loopback port 9,
//! where `connect()` fails immediately and deterministically (nothing
//! listens), exercising only the error path of `signed(..)`.

use coxagent_application::ports::outbound::StoragePort;
use coxagent_infrastructure::storage::S3Storage;

/// An S3 adapter aimed at a loopback port with no listener: every request
/// fails at connect time, deterministically.
fn unreachable_store() -> S3Storage {
    S3Storage::new(
        "http://127.0.0.1:9".to_owned(),
        "coxagent-test".to_owned(),
        "us-east-1".to_owned(),
        "test-access-key".to_owned(),
        "test-secret-key".to_owned(),
    )
}

#[tokio::test]
async fn exists_is_false_when_the_object_store_is_unreachable() {
    let store = unreachable_store();
    assert!(
        !store.exists("screenshots/audit.png").await,
        "exists() must answer false on a transport error, never true/panic"
    );
}

#[tokio::test]
async fn exists_is_false_for_keys_that_could_escape_the_bucket() {
    // Pure branch: `safe_key` rejects before any IO is attempted, so these
    // are deterministic regardless of network state.
    let store = unreachable_store();
    assert!(!store.exists("../escape").await);
    assert!(!store.exists("/absolute").await);
    assert!(!store.exists("").await);
}
