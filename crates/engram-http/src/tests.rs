use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use engram_core::{Engine, FakeEmbedder, Store};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::app;

fn test_app() -> Router {
    let engine = Engine::new(
        Store::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    app(engine)
}

async fn req(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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

fn decision(title: &str, body: &str) -> Value {
    json!({
        "type": "Decision",
        "title": title,
        "body": body,
        "durability": "stable",
        "source": "claude"
    })
}

#[tokio::test]
async fn health_ok() {
    let app = test_app();
    let (status, _) = req(&app, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn drift_lists_nodes_with_missing_code_refs() {
    let app = test_app();
    let (_, node) = req(
        &app,
        "POST",
        "/nodes",
        Some(json!({
            "type": "Caution",
            "title": "refs moved",
            "durability": "stable",
            "source": "claude",
            // Cargo.toml exists in the test cwd; the other path nowhere.
            "code_refs": ["Cargo.toml", "src/definitely-gone.rs"]
        })),
    )
    .await;
    let id = node["id"].as_str().unwrap();

    let (status, drifted) = req(&app, "GET", "/drift", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = drifted.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], id);
    assert_eq!(list[0]["missing"], json!(["src/definitely-gone.rs"]));
}

#[tokio::test]
async fn create_get_and_missing_node() {
    let app = test_app();
    let (status, node) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("Use Rust", "backend")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(node["type"], "Decision");
    let id = node["id"].as_str().unwrap();

    let (status, got) = req(&app, "GET", &format!("/nodes/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["title"], "Use Rust");

    let (status, _) = req(&app, "GET", "/nodes/missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn search_finds_created_node() {
    let app = test_app();
    req(
        &app,
        "POST",
        "/nodes",
        Some(decision("Adopt SQLite WAL", "concurrent reads")),
    )
    .await;
    let (status, hits) = req(&app, "GET", "/search?q=sqlite", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["title"], "Adopt SQLite WAL");
}

#[tokio::test]
async fn edges_and_dangling_endpoint() {
    let app = test_app();
    let (_, a) = req(&app, "POST", "/nodes", Some(decision("A", ""))).await;
    let (_, b) = req(
        &app,
        "POST",
        "/nodes",
        Some(json!({
            "type": "Principle", "title": "B", "durability": "stable", "source": "user"
        })),
    )
    .await;
    let (aid, bid) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());

    let (status, edge) = req(
        &app,
        "POST",
        "/edges",
        Some(json!({
            "type": "because", "from_id": aid, "to_id": bid, "source": "claude"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edge["type"], "because");

    let (status, edges) = req(&app, "GET", &format!("/nodes/{aid}/edges"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edges["out"].as_array().unwrap().len(), 1);

    // dangling endpoint -> 404
    let (status, _) = req(
        &app,
        "POST",
        "/edges",
        Some(json!({
            "type": "because", "from_id": aid, "to_id": "ghost", "source": "claude"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_then_delete() {
    let app = test_app();
    let (_, n) = req(&app, "POST", "/nodes", Some(json!({
        "type": "Problem", "title": "flaky", "durability": "episodic", "source": "claude", "status": "open"
    }))).await;
    let id = n["id"].as_str().unwrap();

    let (status, updated) = req(
        &app,
        "PATCH",
        &format!("/nodes/{id}"),
        Some(json!({ "status": "resolved" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["status"], "resolved");

    let (status, _) = req(&app, "DELETE", &format!("/nodes/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = req(&app, "GET", &format!("/nodes/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // deleting again -> 404
    let (status, _) = req(&app, "DELETE", &format!("/nodes/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_open_and_graph() {
    let app = test_app();
    req(&app, "POST", "/nodes", Some(json!({
        "type": "Intent", "title": "do later", "durability": "volatile", "source": "claude", "status": "open"
    }))).await;
    let (_, a) = req(&app, "POST", "/nodes", Some(decision("A", ""))).await;
    let (_, b) = req(&app, "POST", "/nodes", Some(decision("B", ""))).await;
    req(
        &app,
        "POST",
        "/edges",
        Some(json!({
            "type": "replaces", "from_id": b["id"], "to_id": a["id"], "source": "claude"
        })),
    )
    .await;

    let (status, open) = req(&app, "GET", "/open", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(open.as_array().unwrap().len(), 1);
    assert_eq!(open[0]["type"], "Intent");

    let (status, graph) = req(&app, "GET", "/graph", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn export_then_import_into_fresh_app() {
    let app = test_app();
    let (_, a) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("Use Rust backend", "rmcp")),
    )
    .await;
    let (_, b) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("Local-first", "own data")),
    )
    .await;
    req(
        &app,
        "POST",
        "/edges",
        Some(json!({
            "type": "because", "from_id": a["id"], "to_id": b["id"], "source": "claude"
        })),
    )
    .await;

    let (status, snap) = req(&app, "GET", "/export", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(snap["version"], 1);
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(snap["edges"].as_array().unwrap().len(), 1);

    // import the snapshot into a brand new app
    let app2 = test_app();
    let (status, summary) = req(&app2, "POST", "/import", Some(snap)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["nodes"], 2);
    assert_eq!(summary["edges"], 1);

    // same ids retrievable + searchable in the fresh app
    let (status, got) = req(
        &app2,
        "GET",
        &format!("/nodes/{}", a["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["title"], "Use Rust backend");
    let (_, hits) = req(&app2, "GET", "/search?q=rust", None).await;
    assert_eq!(hits[0]["id"], a["id"]);
}

#[tokio::test]
async fn reconfirm_stamps_last_seen_and_approve_maxes_trust() {
    let app = test_app();
    // claude node starts on the unseen curve (trust 0.5, unapproved)
    let (_, n) = req(&app, "POST", "/nodes", Some(decision("Provisional", "x"))).await;
    let id = n["id"].as_str().unwrap();
    assert!((n["trust"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    assert!(n["approved_at"].is_null());

    let (status, got) = req(&app, "POST", &format!("/nodes/{id}/reconfirm"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!got["last_seen"].is_null(), "reconfirm stamps last_seen");
    assert!(got["trust"].as_f64().unwrap() > 0.55);

    let (status, approved) = req(&app, "POST", &format!("/nodes/{id}/approve"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!approved["approved_at"].is_null());
    assert!(approved["trust"].as_f64().unwrap() > 0.99);
    assert_eq!(approved["stale"], false);

    let (status, _) = req(&app, "POST", "/nodes/ghost/approve", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pin_revoke_and_confirm_roundtrip() {
    let app = test_app();
    let (_, n) = req(&app, "POST", "/nodes", Some(decision("Pinnable", "x"))).await;
    let id = n["id"].as_str().unwrap();

    // Pin: constant trust, stale off, survives everything until unpinned.
    let (status, pinned) = req(
        &app,
        "POST",
        &format!("/nodes/{id}/pin"),
        Some(json!({ "value": 1.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pinned["trust_override"].as_f64().unwrap(), 1.0);
    assert_eq!(pinned["trust"].as_f64().unwrap(), 1.0);

    // Arbitrary constant values are allowed and clamped to 0..=1.
    let (_, odd) = req(
        &app,
        "POST",
        &format!("/nodes/{id}/pin"),
        Some(json!({ "value": 1.4 })),
    )
    .await;
    assert_eq!(odd["trust_override"].as_f64().unwrap(), 1.0);

    // null clears the pin.
    let (_, unpinned) = req(
        &app,
        "POST",
        &format!("/nodes/{id}/pin"),
        Some(json!({ "value": null })),
    )
    .await;
    assert!(unpinned["trust_override"].is_null());

    // Revoke approval: drops approved_at (and any pin) back to the anchor.
    req(&app, "POST", &format!("/nodes/{id}/approve"), None).await;
    req(
        &app,
        "POST",
        &format!("/nodes/{id}/pin"),
        Some(json!({ "value": 1.0 })),
    )
    .await;
    let (status, revoked) = req(&app, "DELETE", &format!("/nodes/{id}/approve"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(revoked["approved_at"].is_null());
    assert!(revoked["trust_override"].is_null());

    let (status, _) = req(
        &app,
        "POST",
        "/nodes/ghost/pin",
        Some(json!({ "value": 1.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_node_type_is_400() {
    let app = test_app();
    let (status, _) = req(&app, "GET", "/search?q=x&types=Nonsense", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn redaction_through_api() {
    let app = test_app();
    let (_, node) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("leak", "key AKIAIOSFODNN7EXAMPLE end")),
    )
    .await;
    let id = node["id"].as_str().unwrap();
    let (_, got) = req(&app, "GET", &format!("/nodes/{id}"), None).await;
    assert!(got["body"].as_str().unwrap().contains("[REDACTED]"));
    assert!(!got["body"].as_str().unwrap().contains("AKIA"));
}

#[tokio::test]
async fn brief_returns_markdown_digest() {
    let app = test_app();
    req(
        &app,
        "POST",
        "/nodes",
        Some(decision("Backend in Rust", "rmcp")),
    )
    .await;

    let request = Request::builder()
        .method("GET")
        .uri("/brief?max_chars=2000")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("text/markdown"));
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.starts_with("# Engram brief"));
    assert!(text.contains("Backend in Rust"));
    assert!(text.len() <= 2000);
}

#[tokio::test]
async fn edge_patch_and_delete() {
    let app = test_app();
    let (_, a) = req(&app, "POST", "/nodes", Some(decision("A", ""))).await;
    let (_, b) = req(&app, "POST", "/nodes", Some(decision("B", ""))).await;
    let (status, edge) = req(
        &app,
        "POST",
        "/edges",
        Some(json!({
            "type": "conflicts-with",
            "from_id": a["id"], "to_id": b["id"], "source": "claude"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = edge["id"].as_str().unwrap();

    let (status, patched) = req(
        &app,
        "PATCH",
        &format!("/edges/{id}"),
        Some(json!({ "status": "resolved", "note": "settled" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["status"], "resolved");
    assert_eq!(patched["note"], "settled");

    let (status, _) = req(&app, "DELETE", &format!("/edges/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = req(&app, "DELETE", &format!("/edges/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn search_hits_include_neighbors() {
    let app = test_app();
    let (_, a) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("adopt sqlite storage", "")),
    )
    .await;
    let (_, b) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("adopt postgres storage", "")),
    )
    .await;
    req(
        &app,
        "POST",
        "/edges",
        Some(json!({
            "type": "replaces",
            "from_id": b["id"], "to_id": a["id"], "source": "claude"
        })),
    )
    .await;

    let (status, hits) = req(
        &app,
        "GET",
        "/search?q=adopt%20sqlite%20storage&limit=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hit = &hits.as_array().unwrap()[0];
    let neighbors = hit["neighbors"].as_array().unwrap();
    assert!(!neighbors.is_empty());
    assert_eq!(neighbors[0]["edge_type"], "replaces");
}

#[tokio::test]
async fn conflict_scan_flow_over_http() {
    let app = test_app();
    let (_, a) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("use postgres for storage", "")),
    )
    .await;
    let (_, _b) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision("use postgres for storage!", "")),
    )
    .await;

    let (status, scanned) = req(&app, "POST", "/conflicts/scan", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(scanned["added"], 1);

    let (status, suspects) = req(&app, "GET", "/conflicts/suspects", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = suspects.as_array().unwrap();
    assert_eq!(list.len(), 1);
    let sid = list[0]["id"].as_str().unwrap();
    assert!(list[0]["similarity"].as_f64().unwrap() > 0.7);

    let (status, resolved) = req(
        &app,
        "POST",
        &format!("/conflicts/suspects/{sid}/resolve"),
        Some(json!({ "verdict": "conflict" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resolved["edge"]["type"], "conflicts-with");
    assert_eq!(resolved["edge"]["source"], "user");

    let (_, after) = req(&app, "GET", "/conflicts/suspects", None).await;
    assert!(after.as_array().unwrap().is_empty());
    let _ = a;
}

#[tokio::test]
async fn decay_endpoint_previews_and_archives() {
    let app = test_app();
    let (_, node) = req(
        &app,
        "POST",
        "/nodes",
        Some(json!({
            "type": "Insight",
            "title": "temporary workaround",
            "durability": "episodic",
            "source": "claude"
        })),
    )
    .await;
    let id = node["id"].as_str().unwrap();

    // Fresh node: nothing decays even without dry_run.
    let (status, out) = req(&app, "POST", "/decay?ttl_days=14", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["archived"], 0);

    let (_, dry) = req(&app, "POST", "/decay?ttl_days=14&dry_run=true", None).await;
    assert_eq!(dry["ids"].as_array().unwrap().len(), 0);
    let _ = id;
}

#[tokio::test]
async fn tags_endpoint_lists_vocabulary_and_edge_retype_works() {
    let app = test_app();
    let (_, a) = req(
        &app,
        "POST",
        "/nodes",
        Some(json!({
            "type": "Decision", "title": "A", "durability": "stable",
            "source": "user", "tags": ["Phase 1", "ui"]
        })),
    )
    .await;
    let (_, b) = req(
        &app,
        "POST",
        "/nodes",
        Some(json!({
            "type": "Principle", "title": "B", "durability": "stable",
            "source": "user", "tags": ["phase-1"]
        })),
    )
    .await;
    assert_eq!(
        a["tags"],
        json!(["phase-1", "ui"]),
        "tags normalized on write"
    );

    let (status, tags) = req(&app, "GET", "/tags", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = tags.as_array().unwrap();
    assert_eq!(list.len(), 2);
    let phase = list.iter().find(|t| t["tag"] == "phase-1").unwrap();
    assert_eq!(phase["count"], 2);

    // Edge retype from the pane: PATCH {type} rewrites the verb in place.
    let (_, edge) = req(
        &app,
        "POST",
        "/edges",
        Some(json!({
            "type": "about", "from_id": a["id"], "to_id": b["id"], "source": "user"
        })),
    )
    .await;
    let (status, patched) = req(
        &app,
        "PATCH",
        &format!("/edges/{}", edge["id"].as_str().unwrap()),
        Some(json!({ "type": "because" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["type"], "because");
}

#[tokio::test]
async fn audit_endpoint_pages_the_journal_with_pane_origin() {
    let app = test_app();
    let (_, node) = req(&app, "POST", "/nodes", Some(decision("first", "a"))).await;
    let id = node["id"].as_str().unwrap().to_string();
    req(
        &app,
        "PATCH",
        &format!("/nodes/{id}"),
        Some(json!({ "status": "resolved" })),
    )
    .await;
    req(
        &app,
        "POST",
        "/nodes",
        Some(decision("second unrelated", "b")),
    )
    .await;

    let (status, page) = req(&app, "GET", "/audit", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 3);
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    // Newest first: created(second), updated(first), created(first).
    assert_eq!(entries[0]["action"], "created");
    assert_eq!(entries[1]["action"], "updated");
    assert_eq!(entries[2]["action"], "created");
    assert_eq!(entries[1]["entity_id"], id.as_str());
    assert_eq!(
        entries[1]["origin"], "pane",
        "HTTP writes attribute to the pane"
    );
    assert_eq!(entries[1]["before"]["status"], Value::Null);
    assert_eq!(entries[1]["after"]["status"], "resolved");
    assert!(entries[0]["cwd"].is_string());
    assert!(entries[0]["pid"].is_number());
    assert!(entries[0]["version"].is_string());

    // Keyset pagination: limit + before cursor.
    let (_, p1) = req(&app, "GET", "/audit?limit=2", None).await;
    assert_eq!(p1["entries"].as_array().unwrap().len(), 2);
    let cursor = p1["entries"][1]["seq"].as_i64().unwrap();
    let (_, p2) = req(&app, "GET", &format!("/audit?before={cursor}"), None).await;
    let rest = p2["entries"].as_array().unwrap();
    assert_eq!(rest.len(), 1);
    assert!(rest[0]["seq"].as_i64().unwrap() < cursor);

    // Entity filter narrows to one node's history.
    let (_, filtered) = req(&app, "GET", &format!("/audit?entity_id={id}"), None).await;
    assert_eq!(filtered["total"], 2);
    assert!(
        filtered["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["entity_id"] == id.as_str())
    );
}

#[tokio::test]
async fn system_reports_version_store_and_wiring() {
    // Mirror real daemon startup: build_engine stamps the embed composition.
    let engine = Engine::new(
        Store::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    engine.ensure_embed_composition().unwrap();
    let app = app(engine);
    req(&app, "POST", "/nodes", Some(decision("one", "body"))).await;

    let (status, v) = req(&app, "GET", "/system", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["store"]["nodes"], 1);
    assert_eq!(v["store"]["journal_mode"], "memory"); // in-memory test store
    assert_eq!(v["store"]["integrity_ok"], true);
    assert!(v["store"]["embed_composition_current"].as_bool().unwrap());
    assert!(v["wiring"].is_array());
    assert!(v["daemon"]["pid"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn digest_scan_walks_the_repo_root_derived_from_the_db_path() {
    // The daemon derives the scan root from the served DB path
    // (<root>/.engram/graph.db), same as drift.
    let root = std::env::temp_dir().join(format!("engram-http-digest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(".gitignore"), "gen/\n").unwrap();
    std::fs::create_dir_all(root.join("gen")).unwrap();
    std::fs::write(root.join("gen/out.rs"), "// FIXME ignored\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "// FIXME empty input crashes\n").unwrap();

    let engine = Engine::new(
        Store::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let app = crate::router_shared_with_db(
        std::sync::Arc::new(std::sync::Mutex::new(engine)),
        root.join(".engram/graph.db").display().to_string(),
    );

    let (status, body) = req(&app, "POST", "/digest/scan", None).await;
    assert_eq!(status, StatusCode::OK);
    let candidates = body["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["marker"], "FIXME");
    assert_eq!(candidates[0]["suggested_type"], "Problem");
    assert_eq!(candidates[0]["file"], "src/lib.rs");
    assert_eq!(body["truncated"], false);

    let _ = std::fs::remove_dir_all(&root);
}
