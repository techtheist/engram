use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use engram_core::{Engine, FakeEmbedder, SqliteStore};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::app;

pub(super) fn test_app() -> Router {
    let engine = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    app(engine)
}

pub(super) async fn req(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, val)
}

pub(super) fn decision(title: &str, body: &str) -> Value {
    json!({
        "type": "Decision",
        "title": title,
        "body": body,
        "durability": "stable",
        "source": "claude"
    })
}

mod api;
mod projects;
mod surfaces;
