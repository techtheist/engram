//! Project scoping: `/projects/{id}/…` routes over a hub.

use super::*;
use engram_core::{EngineFactory, Hub, registry};
use std::sync::{Arc, Mutex};

fn hub_app(tmp: &std::path::Path) -> Router {
    let factory: EngineFactory = Box::new(|db: &std::path::Path| {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| engram_core::Error::Io(e.to_string()))?;
        }
        Ok(Engine::new(
            SqliteStore::open(db)?,
            Box::new(FakeEmbedder::default()),
        ))
    });
    let alpha_root = tmp.join("alpha");
    let alpha_db = alpha_root.join(".engram/graph.db");
    std::fs::create_dir_all(alpha_db.parent().unwrap()).unwrap();
    let alpha = registry::register(&alpha_root, &alpha_db).unwrap();
    let engine = Engine::new(
        SqliteStore::open(&alpha_db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    crate::router_hub(
        Arc::new(Hub::new(
            Arc::new(Mutex::new(engine)),
            Some(alpha),
            Some(factory),
        )),
        None,
    )
}

/// One test on purpose: ENGRAM_HOME is process-wide, so the scoped-route
/// story runs sequentially inside its sandbox.
#[tokio::test]
async fn scoped_routes_reach_other_projects_and_all_stays_read_only() {
    let tmp = std::env::temp_dir().join(format!("engram-http-hub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut env = engram_core::sync::EnvGuard::lock();
    env.set("ENGRAM_HOME", tmp.join("enghome"));
    let beta_root = tmp.join("beta");
    std::fs::create_dir_all(&beta_root).unwrap();
    registry::register(&beta_root, &beta_root.join(".engram/graph.db")).unwrap();
    let app = hub_app(&tmp);

    // The registry listing carries current + home + siblings.
    let (status, projects) = req(&app, "GET", "/projects", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<String> = projects
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"alpha".into()), "got: {names:?}");
    assert!(names.contains(&"beta".into()), "got: {names:?}");
    assert!(names.contains(&"home".into()), "got: {names:?}");

    // A scoped write lands in the sibling's graph, not the current one.
    let (status, _) = req(
        &app,
        "POST",
        "/projects/beta/nodes",
        Some(decision("beta scoped note", "written through the hub")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, graph) = req(&app, "GET", "/projects/beta/graph", None).await;
    assert!(
        graph["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("beta graph response: {graph}"))
            .iter()
            .any(|n| n["title"] == "beta scoped note")
    );
    let (_, graph) = req(&app, "GET", "/graph", None).await;
    assert!(
        !graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["title"] == "beta scoped note"),
        "the current project must not see the sibling's write"
    );

    // The home graph accepts deliberate writes.
    let (status, _) = req(
        &app,
        "POST",
        "/projects/home/nodes",
        Some(json!({
            "type": "Principle",
            "title": "user-level canon lives at home",
            "durability": "stable",
            "source": "user"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // `all` reads fan out with provenance…
    let (status, res) = req(
        &app,
        "GET",
        "/projects/all/search?q=beta%20scoped%20note",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        res["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["project"] == "beta"),
        "got: {res}"
    );

    // …and `all` writes are refused, pointing at home.
    let (status, err) = req(
        &app,
        "POST",
        "/projects/all/nodes",
        Some(decision("fan out", "nope")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        err["error"].as_str().unwrap().contains("home"),
        "got: {err}"
    );

    // Unregistering withdraws awareness only.
    let (status, _) = req(&app, "DELETE", "/projects/beta", None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, projects) = req(&app, "GET", "/projects", None).await;
    assert!(
        !projects
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "beta")
    );
    // The entry named graph.db, but a store born lazily through the hub
    // is born on the RESOLVED path (tepin since v0.6.2) — unregistering
    // must leave that file behind.
    assert!(
        engram_core::resolve_db_path(&beta_root.join(".engram/graph.db")).is_file(),
        "the data itself is untouched"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
