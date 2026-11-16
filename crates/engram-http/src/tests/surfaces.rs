//! Folder picker, backends, model admin, CORS, tasks and review routes.

use super::*;

#[tokio::test]
async fn fs_dirs_lists_directories_for_the_folder_picker() {
    let tmp = std::env::temp_dir().join(format!("engram-fsdirs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("visible")).unwrap();
    std::fs::create_dir_all(tmp.join(".hidden")).unwrap();
    std::fs::create_dir_all(tmp.join("proj/.engram")).unwrap();
    std::fs::write(tmp.join("proj/.engram/graph.db"), b"").unwrap();
    std::fs::write(tmp.join("a-file.txt"), b"not a dir").unwrap();

    let app = test_app();
    let (status, listing) = req(
        &app,
        "GET",
        &format!("/fs/dirs?path={}", urlencode(&tmp.display().to_string())),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = listing["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["proj", "visible"],
        "sorted dirs only, no dot-dirs, no files"
    );
    let proj = &listing["dirs"][0];
    assert_eq!(proj["engram"], true, "existing graphs are flagged");
    assert!(listing["parent"].is_string());

    let (status, err) = req(&app, "GET", "/fs/dirs?path=/definitely/not/a/dir", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got: {err}");

    let _ = std::fs::remove_dir_all(&tmp);
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || b"-_./~".contains(&b) {
                vec![b as char]
            } else {
                format!("%{b:02X}").chars().collect()
            }
        })
        .collect()
}

#[tokio::test]
async fn tepin_backend_serves_the_full_api_surface() {
    // The §7C regression gate in http form: the same router, a TepinDB store.
    let engine = Engine::new(
        engram_core::TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let app = app(engine);

    let (status, node) = req(
        &app,
        "POST",
        "/nodes",
        Some(decision(
            "served from tepindb",
            "driver swap behind the trait",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = node["id"].as_str().unwrap().to_string();

    let (status, hits) = req(&app, "GET", "/search?q=tepindb", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits.as_array().unwrap()[0]["id"], json!(id));

    let (status, system) = req(&app, "GET", "/system", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(system["store"]["backend"], json!("tepindb"));
    assert_eq!(system["store"]["nodes"], json!(1));

    let (status, audit) = req(&app, "GET", "/audit", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        audit["total"].as_i64().unwrap() >= 1,
        "writes journal on tepin"
    );
}

// ---- model selection endpoints --------------------------------

/// A canned admin so the endpoint tests need no real downloads or models.
struct FakeAdmin;

impl crate::ModelAdmin for FakeAdmin {
    fn describe(&self) -> Value {
        json!({ "available": true, "roles": [{ "role": "nli", "active": "fake-nli" }] })
    }
    fn apply(&self, request: Value) -> engram_core::Result<Value> {
        match request["role"].as_str() {
            Some("nli") => Ok(json!({ "role": "nli", "applied": "fake-nli-2" })),
            other => Err(engram_core::Error::Project(format!(
                "unknown role {other:?}"
            ))),
        }
    }
}

#[tokio::test]
async fn models_endpoint_without_admin_reports_unavailable() {
    // Library/test embeddings have no model-selection hands: GET degrades
    // honestly, POST refuses with a client error.
    let app = test_app();
    let (status, body) = req(&app, "GET", "/models", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], json!(false));
    let (status, _) = req(&app, "POST", "/models", Some(json!({ "role": "nli" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn models_endpoint_with_admin_describes_and_applies() {
    let engine = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hub = std::sync::Arc::new(engram_core::Hub::single(engine));
    let app = crate::router_hub_with_models(hub, None, std::sync::Arc::new(FakeAdmin));

    let (status, body) = req(&app, "GET", "/models", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], json!(true));
    assert_eq!(body["roles"][0]["active"], json!("fake-nli"));

    let (status, body) = req(&app, "POST", "/models", Some(json!({ "role": "nli" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], json!("fake-nli-2"));

    // The admin's refusal maps to a 400, not a 500 — it's the user's input.
    let (status, body) = req(&app, "POST", "/models", Some(json!({ "role": "bogus" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("unknown role"));
}

#[tokio::test]
async fn cors_allows_local_ui_origins_and_refuses_the_open_web() {
    // SECURITY.md hardening: a random website in the user's browser must not
    // be able to read the graph API; the pane's own origins must.
    let app = test_app();
    let probe = |origin: &'static str| {
        let app = app.clone();
        async move {
            let req = Request::builder()
                .method("GET")
                .uri("/health")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap().to_string())
        }
    };
    assert_eq!(
        probe("http://127.0.0.1:8787").await.as_deref(),
        Some("http://127.0.0.1:8787")
    );
    assert_eq!(
        probe("http://localhost:5173").await.as_deref(),
        Some("http://localhost:5173")
    );
    assert!(probe("vscode-webview://abc123").await.is_some());
    assert_eq!(probe("https://evil.example").await, None);
    assert_eq!(probe("http://127.0.0.1.evil.example").await, None);
}

/// The pane's task board API: create, group, patch with clears,
/// order, list by status, delete (a group takes its tasks along).
#[tokio::test]
async fn task_board_round_trip() {
    let app = test_app();
    let (status, group) = req(
        &app,
        "POST",
        "/tasks",
        Some(json!({ "title": "Tasks feature" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group}");
    let gid = group["id"].as_str().unwrap().to_string();
    let (_, a) = req(
        &app,
        "POST",
        "/tasks",
        Some(json!({ "title": "Board screen", "parent": gid, "close": "in 3 days" })),
    )
    .await;
    let aid = a["id"].as_str().unwrap().to_string();
    let (_, b) = req(
        &app,
        "POST",
        "/tasks",
        Some(json!({ "title": "Brief section", "parent": gid })),
    )
    .await;
    let bid = b["id"].as_str().unwrap().to_string();

    let (status, bad) = req(
        &app,
        "POST",
        "/tasks",
        Some(json!({ "title": "x", "wake": "someday" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");

    let (status, patched) = req(
        &app,
        "PATCH",
        &format!("/tasks/{aid}"),
        Some(json!({ "close": "", "status": "delayed", "progress": "waiting on design" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert!(patched.get("close").is_none(), "\"\" clears: {patched}");
    assert_eq!(patched["status"], "delayed");

    let (status, _) = req(
        &app,
        "POST",
        "/tasks/order",
        Some(json!({ "ids": [bid, aid] })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, open) = req(
        &app,
        "GET",
        &format!("/tasks?status=open&parent={gid}"),
        None,
    )
    .await;
    let titles: Vec<&str> = open
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["Brief section", "Board screen"],
        "active before delayed"
    );

    let (_, all) = req(&app, "GET", "/tasks", None).await;
    let g = all
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == gid)
        .unwrap();
    assert_eq!(g["progress"], json!([0, 2]));

    let (status, _) = req(&app, "DELETE", &format!("/tasks/{gid}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, all) = req(&app, "GET", "/tasks", None).await;
    assert_eq!(all, json!([]));
    let (status, _) = req(&app, "GET", &format!("/tasks/{aid}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Crowding and the review queue over HTTP — the pane's heat view
/// and Checkup section. Plumbing only: clustering quality is engram-core's.
#[tokio::test]
async fn crowding_review_and_marks_round_trip() {
    let app = test_app();
    let mut ids = Vec::new();
    for title in [
        "Bridges reconnect after the core restarts",
        "Bridges reconnect after the core restarts, twice",
        "Something unrelated about the pane",
    ] {
        let (status, node) = req(&app, "POST", "/nodes", Some(decision(title, "x"))).await;
        assert_eq!(status, StatusCode::OK, "{node}");
        ids.push(node["id"].as_str().unwrap().to_string());
    }

    let (status, report) = req(&app, "GET", "/crowding", None).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["size"], 3);
    assert!(report["nodes"][&ids[0]]["heat"].is_number(), "{report}");
    assert!(report["subjects"].is_array() && report["min_size"] == 5);

    let (status, page) = req(&app, "GET", "/review?limit=5", None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page["band"].is_number() && page["subjects"].is_array());

    let (status, marked) = req(
        &app,
        "POST",
        "/review/marks",
        Some(json!({ "ids": [ids[0], "missing"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{marked}");
    assert_eq!(marked["marked"], json!([ids[0]]));
    assert_eq!(marked["reviewed"], true);

    // A reviewed island leaves the report's island list and the page.
    let (_, report) = req(&app, "GET", "/crowding", None).await;
    assert_eq!(report["islands"].as_array().unwrap().len(), 2, "{report}");
    let (status, islands) = req(&app, "GET", "/review/islands", None).await;
    assert_eq!(status, StatusCode::OK, "{islands}");
    assert_eq!(
        (islands["total"].clone(), islands["hidden_reviewed"].clone()),
        (json!(2), json!(1))
    );
    let (status, conflicts) = req(&app, "GET", "/review/conflicts", None).await;
    assert_eq!(status, StatusCode::OK, "{conflicts}");
    assert!(conflicts["standing"].is_array() && conflicts["suspects"].is_array());

    let (status, cleared) = req(
        &app,
        "POST",
        "/review/marks",
        Some(json!({ "ids": [ids[0]], "reviewed": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["reviewed"], false);
}
