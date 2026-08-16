//! CXA-F023 end-to-end: a booted hub answers GET /api/openapi.json with an
//! OpenAPI 3.x document describing its service routes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

/// Fixed, high, unique port so this file does not race other hub-booting tests.
const PORT_OPEN: u16 = 47_731;

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
    let health = format!("http://127.0.0.1:{port}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn openapi_endpoint_serves_a_valid_openapi_document() {
    serve(PORT_OPEN).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{PORT_OPEN}/api/openapi.json"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "openapi.json must answer 200");

    let doc: serde_json::Value =
        serde_json::from_str(&resp.text().await.unwrap()).expect("valid JSON document");

    assert!(doc["openapi"]
        .as_str()
        .unwrap_or_default()
        .starts_with("3."));
    assert_eq!(doc["info"]["title"], "CoXAgent Hub API");
    assert!(
        !doc["paths"].as_object().unwrap().is_empty(),
        "spec describes at least one service route"
    );
}
