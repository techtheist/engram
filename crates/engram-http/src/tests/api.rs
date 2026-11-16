//! The single-project API surface.

use super::*;

#[tokio::test]
async fn health_ok() {
    let app = test_app();
    let (status, body) = req(&app, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["pid"].as_u64(),
        Some(u64::from(std::process::id())),
        "health names the answering process: {body}"
    );
}

/// `/settings` is a machine-core surface, like the census: a daemon without
/// a `CoreRuntime` answers 404 on both verbs — which is exactly what tells
/// an older pane to hide the "Default agent project" control. The write path
/// (validation, persistence, the bridge rung) is covered end-to-end in
/// crates/engram-cli/tests/roots_binding.rs against a sandboxed core.
#[tokio::test]
async fn settings_hidden_without_core_runtime() {
    let app = test_app();
    let (status, _) = req(&app, "GET", "/settings", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = req(
        &app,
        "POST",
        "/settings",
        Some(json!({ "default_agent_project": "anything" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
async fn config_get_put_validation_and_400_leaves_store_untouched() {
    let app = test_app();
    let (status, cfg) = req(&app, "GET", "/config", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cfg["ontology"]["preset"], "engram");
    assert_eq!(cfg["brief"]["suspects"]["cap"], 8);
    assert_eq!(cfg["policy"]["conflict_suspect_similarity"], 0.88);

    let mut modified = cfg.clone();
    modified["brief"]["recent"]["cap"] = json!(3);
    modified["ontology"]["preset"] = json!("custom");
    let (status, echoed) = req(&app, "PUT", "/config", Some(modified)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(echoed["brief"]["recent"]["cap"], 3);
    let (_, got) = req(&app, "GET", "/config", None).await;
    assert_eq!(got["brief"]["recent"]["cap"], 3);
    assert_eq!(got["ontology"]["preset"], "custom");

    // An invariant break is a 400 with the rule in the message, and the
    // stored document is untouched.
    let mut broken = got.clone();
    broken["ontology"]["types"][0]["hue"] = json!(720);
    let (status, err) = req(&app, "PUT", "/config", Some(broken)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("hue"));
    let (_, still) = req(&app, "GET", "/config", None).await;
    assert_eq!(
        still["ontology"]["types"][0]["hue"],
        cfg["ontology"]["types"][0]["hue"]
    );
    assert_eq!(
        still["brief"]["recent"]["cap"], 3,
        "the last valid config survives"
    );
}

#[tokio::test]
async fn applying_a_preset_swaps_the_ontology_and_keeps_the_rest() {
    let app = test_app();
    let (_, mut cfg) = req(&app, "GET", "/config", None).await;
    cfg["policy"]["conflict_suspect_similarity"] = json!(0.8897);
    cfg["brief"]["recent"]["cap"] = json!(3);
    let (status, _) = req(&app, "PUT", "/config", Some(cfg)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, applied) = req(&app, "POST", "/config/presets/general", None).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["ontology"]["preset"], "general");
    let (_, got) = req(&app, "GET", "/config", None).await;
    assert_eq!(got["ontology"]["preset"], "general");
    assert_eq!(got["policy"]["conflict_suspect_similarity"], 0.8897);
    assert_eq!(got["brief"]["recent"]["cap"], 3);

    let (status, err) = req(&app, "POST", "/config/presets/nope", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("unknown preset"));
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

/// `GET /guide` is the operator's manual: markdown, and every route
/// it names as `` `METHOD /path` `` must exist on the router — a recipe that
/// points at a route that isn't there teaches the assistant a 404.
#[tokio::test]
async fn guide_is_markdown_and_names_only_real_routes() {
    let app = test_app();
    let request = Request::builder()
        .method("GET")
        .uri("/guide")
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
    assert!(ct.starts_with("text/markdown"), "{ct}");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.starts_with("# Engram operator's guide"));
    for must in [
        "PUT /config",
        "brief.ontology.show",
        "history",
        "POST /models",
        "POST /skills/install",
        "Before a digest",
    ] {
        assert!(text.contains(must), "the guide covers {must}");
    }

    // Every `METHOD /path` the guide names (placeholders excluded) is a
    // route: anything but 404. `/settings` legitimately 404s here — the
    // test app has no core runtime — so it is the one allowed miss.
    let mut checked = 0;
    for token in text.split('`').skip(1).step_by(2) {
        let Some((method, path)) = token.split_once(' ') else {
            continue;
        };
        if !matches!(method, "GET" | "PUT" | "POST" | "DELETE") || !path.starts_with('/') {
            continue;
        }
        let path = path.split([' ', '?']).next().unwrap();
        if path.contains('{') || path.contains('<') {
            continue;
        }
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let status = app.clone().oneshot(request).await.unwrap().status();
        // 405 counts as a miss too: `GET /nodes` once slipped through here
        // because the router serves POST /nodes (found by following the
        // guide on a real graph, 0.9.9).
        assert!(
            (status != StatusCode::NOT_FOUND && status != StatusCode::METHOD_NOT_ALLOWED)
                || path == "/settings",
            "the guide names {method} {path}, which the router doesn't serve ({status})"
        );
        checked += 1;
    }
    assert!(
        checked >= 20,
        "the guide names at least twenty routes: {checked}"
    );
}

/// `GET /brief?project=` is "brief me as a session bound there" — the shape
/// the SessionStart hook uses, since a hook holds a folder and the machine
/// core's own launch graph is nobody's project.
#[tokio::test]
async fn brief_takes_a_project_selector_and_refuses_an_unknown_one() {
    let app = test_app();
    req(
        &app,
        "POST",
        "/nodes",
        Some(decision("Backend in Rust", "rmcp")),
    )
    .await;

    let get = async |uri: &str| {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    };

    // A single-project app knows exactly one selector — its own identity.
    // Names, ids and directories all land in the same resolver (covered on
    // the hub in engram-core); what matters here is that the parameter is
    // plumbed and that an unresolvable one fails loudly.
    let resp = get("/brief?project=current&max_chars=2000").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("Backend in Rust"), "{text}");

    // A selector that names nothing is refused, and that refusal is load-
    // bearing: it is how the hook tells a daemon that understands the
    // parameter from an older one that ignores it and answers 200 with its
    // own graph.
    let resp = get("/brief?project=/no/such/project").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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

    // The judged pair feeds the NLI scoreboard; the test engine runs without
    // an NLI model, so the pair is judged-but-unhinted and no rate exists.
    let (status, report) = req(&app, "GET", "/conflicts/agreement", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["judged"], 1);
    assert_eq!(report["with_hint"], 0);
    assert!(report.get("agreement").is_none());
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
async fn audit_undo_reverts_a_row_and_dry_runs_change_nothing() {
    let app = test_app();
    let (_, created) = req(&app, "POST", "/nodes", Some(decision("undo me", "a"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let (_, page) = req(&app, "GET", &format!("/audit?entity_id={id}"), None).await;
    let seq = page["entries"][0]["seq"].as_i64().unwrap();

    let (status, dry) = req(
        &app,
        "POST",
        &format!("/audit/{seq}/undo?dry_run=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dry}");
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["undone"][0]["effect"], "removes the note");
    let (status, _) = req(&app, "GET", &format!("/nodes/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "a dry run leaves the note");

    let (status, done) = req(&app, "POST", &format!("/audit/{seq}/undo"), None).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["undone"].as_array().unwrap().len(), 1);
    let (status, _) = req(&app, "GET", &format!("/nodes/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the note is gone");
    let (_, page) = req(&app, "GET", &format!("/audit?entity_id={id}"), None).await;
    let original = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "created")
        .unwrap();
    assert!(original["undone_by"].is_number(), "the row reads as undone");

    // An unknown session is a 404, not a silent no-op.
    let (status, _) = req(&app, "POST", "/audit/sessions/nope/undo", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn node_standing_names_the_replacement() {
    let app = test_app();
    let (_, old) = req(&app, "POST", "/nodes", Some(decision("old way", "a"))).await;
    let (_, new) = req(&app, "POST", "/nodes", Some(decision("new way", "b"))).await;
    let (old, new) = (old["id"].as_str().unwrap(), new["id"].as_str().unwrap());

    let (status, s) = req(&app, "GET", &format!("/nodes/{old}/standing"), None).await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["verdict"], "unverified");

    let edge = json!({"type": "replaces", "from_id": new, "to_id": old, "source": "claude"});
    let (status, e) = req(&app, "POST", "/edges", Some(edge)).await;
    assert_eq!(status, StatusCode::OK, "{e}");
    let (_, s) = req(&app, "GET", &format!("/nodes/{old}/standing"), None).await;
    assert_eq!(s["verdict"], "superseded", "{s}");
    assert_eq!(s["other"], new);

    let (status, _) = req(&app, "GET", "/nodes/nope/standing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn system_reports_version_store_and_wiring() {
    // Mirror real daemon startup: build_engine stamps the embed composition.
    let engine = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
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
        SqliteStore::open_in_memory().unwrap(),
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
