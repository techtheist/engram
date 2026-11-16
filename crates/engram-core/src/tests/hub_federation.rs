//! The multi-project hub.

use super::*;

/// One test on purpose: it sandboxes ENGRAM_HOME (process-wide env), so the
/// whole federation story runs sequentially inside it — registry, lazy opens,
/// provenance, all-write refusal, the home brief section, and promotions.
#[test]
fn hub_federation_end_to_end() {
    let mut env = crate::sync::EnvGuard::lock();
    let tmp = std::env::temp_dir().join(format!("engram-hub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    env.set("ENGRAM_HOME", tmp.join("enghome"));

    let factory: EngineFactory = Box::new(|db: &std::path::Path| {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(Engine::new(
            SqliteStore::open(db)?,
            Box::new(FakeEmbedder::default()),
        ))
    });

    // A sibling project "beta" on the machine registry.
    let beta_root = tmp.join("beta");
    std::fs::create_dir_all(&beta_root).unwrap();
    let beta_db = beta_root.join(".engram/graph.db");
    let entry = registry::register(&beta_root, &beta_db).unwrap();
    assert_eq!(entry.name, "beta");
    // Re-registration keeps the id: upsert by root, never a duplicate.
    assert_eq!(
        registry::register(&beta_root, &beta_db).unwrap().id,
        entry.id
    );

    // The current project "alpha".
    let alpha_root = tmp.join("alpha");
    let alpha_db = alpha_root.join(".engram/graph.db");
    std::fs::create_dir_all(alpha_db.parent().unwrap()).unwrap();
    let alpha = registry::register(&alpha_root, &alpha_db).unwrap();
    let current = Engine::new(
        SqliteStore::open(&alpha_db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hub = Hub::new(
        std::sync::Arc::new(std::sync::Mutex::new(current)),
        Some(alpha),
        Some(factory),
    );

    // Seed both graphs.
    hub.current_engine()
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Decision,
            "alpha uses tokio for its async runtime",
            "local canon",
        ))
        .unwrap();
    let beta_engine = hub.get("beta").unwrap();
    beta_engine
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Decision,
            "beta uses redb as its storage engine core",
            "sibling canon",
        ))
        .unwrap();

    // 0.8.8: a project's DIRECTORY is a selector too — a session-brief hook
    // or an MCP bridge holds a folder, not a name.
    let beta_by_dir = hub.get(&beta_root.display().to_string()).unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&beta_by_dir, &beta_engine),
        "a directory reaches the engine already open for that project"
    );
    assert_eq!(
        hub.resolve_id(&beta_root.display().to_string()).unwrap(),
        entry.id
    );
    // Any path inside the repo counts, and the launch project answers with
    // the engine it already holds — reopening its store from the registry
    // would be a second handle on the same file.
    let alpha_by_dir = hub
        .get(&alpha_root.join(".engram").display().to_string())
        .unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&alpha_by_dir, &hub.current_engine()),
        "a path inside the launch project is the launch project"
    );
    // Only absolute paths are paths; anything unresolvable is still an error.
    assert!(hub.get("beta/.engram").is_err());
    assert!(
        hub.get(&tmp.join("not-a-project").display().to_string())
            .is_err()
    );

    // Cross-project read: the foreign hit carries provenance.
    let (hits, skipped) = hub.search_all("beta redb storage engine", &[], 8).unwrap();
    assert!(skipped.is_empty(), "{skipped:?}");
    let foreign = hits
        .iter()
        .find(|h| h.project.as_deref() == Some("beta"))
        .expect("the beta hit rides along with provenance");
    assert!(foreign.title.contains("redb"));

    // `all` never resolves to one engine — the refusal points at home.
    let err = hub
        .get("all")
        .err()
        .expect("all must not resolve")
        .to_string();
    assert!(err.contains("home"), "got: {err}");
    // An unknown selector names what exists.
    let err = hub.get("nope").err().expect("unknown selector").to_string();
    assert!(err.contains("beta"), "got: {err}");

    // The home graph: created on first access, briefed into every project.
    let home = hub.get("home").unwrap();
    home.lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "never store secrets anywhere",
            "user-level canon",
        ))
        .unwrap();
    let brief = hub.brief(16000).unwrap();
    assert!(brief.contains("## Home graph"), "got: {brief}");
    assert!(brief.contains("never store secrets"), "got: {brief}");
    // The roster names every other reachable graph, home last.
    assert!(
        brief.contains("## Other project graphs on this machine"),
        "got: {brief}"
    );
    assert!(brief.contains("beta, home"), "got: {brief}");

    // A sibling whose registry entry still names `graph.db` while the store
    // on disk is `graph.tepin` (registered pre-migration, backup cleaned up)
    // must be reached through the same resolution every open applies — the
    // raw-path existence guard used to skip it as "db missing".
    let gamma_root = tmp.join("gamma");
    let gamma_tepin = gamma_root.join(".engram/graph.tepin");
    std::fs::create_dir_all(gamma_tepin.parent().unwrap()).unwrap();
    Engine::new(
        SqliteStore::open(&gamma_tepin).unwrap(),
        Box::new(FakeEmbedder::default()),
    )
    .add_node(new_node(
        NodeType::Decision,
        "gamma renders diagrams with graphviz",
        "sibling canon behind a stale registry path",
    ))
    .unwrap();
    registry::register(&gamma_root, &gamma_root.join(".engram/graph.db")).unwrap();
    let (hits, skipped) = hub.search_all("gamma graphviz diagrams", &[], 8).unwrap();
    assert!(
        skipped.is_empty(),
        "stale db name must resolve: {skipped:?}"
    );
    assert!(
        hits.iter().any(|h| h.project.as_deref() == Some("gamma")),
        "the tepin store behind a graph.db entry is searchable"
    );

    // Promotions: the same Principle in alpha and beta nominates…
    hub.current_engine()
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "always run clippy with warnings denied",
            "shared habit",
        ))
        .unwrap();
    beta_engine
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "always run clippy with warnings denied",
            "shared habit",
        ))
        .unwrap();
    let (candidates, skipped) = hub.promotion_candidates().unwrap();
    assert!(skipped.is_empty(), "{skipped:?}");
    let cand = candidates
        .iter()
        .find(|c| c.node.title.contains("clippy"))
        .expect("recurring Principle nominates for promotion");
    assert_eq!(cand.matches[0].project, "beta");
    // …and a home copy suppresses the nomination (already promoted).
    home.lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "always run clippy with warnings denied",
            "promoted",
        ))
        .unwrap();
    let (candidates, _) = hub.promotion_candidates().unwrap();
    assert!(
        !candidates.iter().any(|c| c.node.title.contains("clippy")),
        "a home copy suppresses the nomination"
    );

    // projects() lists everything with the right flags.
    let projects = hub.projects();
    assert!(projects.iter().any(|p| p.current && p.name == "alpha"));
    assert!(projects.iter().any(|p| p.home));
    assert!(projects.iter().any(|p| p.name == "beta" && p.open));

    // Issue #8: deleting a project's store under a live hub must not leave
    // the cached engine serving the orphaned inode — the registry id
    // survives the wipe (upsert by root), so without an identity check the
    // next get() would cache-hit the corpse and the store would never be
    // recreated on disk.
    let beta_before = hub.get("beta").unwrap();
    std::fs::remove_dir_all(beta_root.join(".engram")).unwrap();
    let beta_after = hub.get("beta").unwrap();
    assert!(
        !std::sync::Arc::ptr_eq(&beta_before, &beta_after),
        "a deleted store evicts the cached engine"
    );
    // The recreated store is tepin-born (resolved_db: fresh graphs get the
    // .tepin path when nothing exists yet), so assert on the resolved path.
    assert!(
        registry::load()
            .resolve("beta")
            .unwrap()
            .resolved_db()
            .exists(),
        "the reopen recreates the store file on disk"
    );
    assert!(
        beta_after
            .lock()
            .unwrap()
            .brief(8000)
            .unwrap()
            .contains("cold start"),
        "the fresh store is empty — a new graph, not the corpse"
    );
    // And an intact store keeps its engine: no eviction churn.
    assert!(std::sync::Arc::ptr_eq(
        &hub.get("beta").unwrap(),
        &beta_after
    ));

    let _ = std::fs::remove_dir_all(&tmp);
}

// ---- Store-trait conformance: both backends, one battery --------
//
