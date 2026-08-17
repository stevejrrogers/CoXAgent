//! CXA-B051 end-to-end: a booted hub answers GET /api/openapi.json with a valid
//! OpenAPI 3.x document describing its service routes, instead of falling
//! through to axum's rejection handler (which returned 404 before this fix).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

async fn serve(port: u16) {
    let dir = tempfile::tempdir().expect("tempdir");
    let extras = coxagent_presentation::HubExtras {
        hub_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        vec![],
        port,
        audit,
        extras,
    ));
    let client = reqwest::Client::new();
    for _ in 0..50 {
        if client
            .get(format!("http://127.0.0.1:{port}/api/health"))
            .send()
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn openapi_endpoint_serves_a_valid_openapi_document() {
    // High, unique port so this file does not race other hub-booting tests.
    let port = 47_731u16;
    serve(port).await;
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/openapi.json"))
        .send()
        .await
        .expect("request openapi.json");
    assert_eq!(resp.status(), 200, "openapi.json must answer 200");
    let body: serde_json::Value = resp.json().await.expect("openapi body is JSON");
    assert!(
        body["openapi"]
            .as_str()
            .unwrap_or_default()
            .starts_with("3."),
        "the document declares an OpenAPI 3.x version"
    );
    assert_eq!(body["info"]["title"], "CoXAgent Hub API");
    let paths = body["paths"].as_object().expect("paths object");
    assert!(
        !paths.is_empty(),
        "spec describes at least one service route"
    );
}
