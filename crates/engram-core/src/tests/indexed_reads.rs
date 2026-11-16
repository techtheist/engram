//! Indexed reads by type and by session.

use super::*;

/// The indexed reads answer exactly what filtering every node answers —
/// through writes, a retype and a delete — on every backend.
pub(super) fn indexed_reads_battery(s: &dyn Store, backend: &str) {
    let mut ids = Vec::new();
    for (i, (t, sess)) in [
        (NodeType::Decision, "s-a"),
        (NodeType::Insight, "s-a"),
        (NodeType::Decision, "s-b"),
        (NodeType::Caution, "s-b"),
        (NodeType::Decision, "s-c"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut n = new_node(t, &format!("note {i} about indexing"), "");
        n.session_id = Some(sess.into());
        ids.push(s.add_node(n).unwrap().id);
    }
    let check = |label: &str| {
        let all = s.all_nodes().unwrap();
        for t in ["Decision", "Insight", "Caution", "Problem"] {
            let want: Vec<String> = all
                .iter()
                .filter(|n| n.node_type.as_str() == t)
                .map(|n| n.id.clone())
                .collect();
            let got: Vec<String> = s
                .nodes_of_type(t)
                .unwrap()
                .into_iter()
                .map(|n| n.id)
                .collect();
            assert_eq!(got, want, "{backend} {label}: nodes_of_type({t})");
        }
        for sess in ["s-a", "s-b", "s-c", "s-none"] {
            let want: Vec<String> = all
                .iter()
                .filter(|n| n.session_id.as_deref() == Some(sess))
                .map(|n| n.id.clone())
                .collect();
            let got: Vec<String> = s
                .nodes_in_session(sess)
                .unwrap()
                .into_iter()
                .map(|n| n.id)
                .collect();
            assert_eq!(got, want, "{backend} {label}: nodes_in_session({sess})");
        }
    };
    check("after writes");
    assert_eq!(s.retype_nodes("Caution", "Problem").unwrap(), 1);
    check("after retype");
    assert!(s.delete_node(&ids[0]).unwrap());
    check("after delete");
}

#[test]
fn indexed_reads_match_full_scans_on_every_backend() {
    indexed_reads_battery(&SqliteStore::open_in_memory().unwrap(), "sqlite");
    indexed_reads_battery(&TepinStore::open_in_memory().unwrap(), "tepindb");
}

#[test]
fn indexed_reads_survive_reopen() {
    let dir = std::env::temp_dir().join(format!("engram-tepin-idx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("graph.tepin");
    let _ = std::fs::remove_file(&path);
    let id = {
        let s = TepinStore::open(&path).unwrap();
        let mut n = new_node(NodeType::Insight, "reopened and still indexed", "");
        n.session_id = Some("sess-1".into());
        s.add_node(n).unwrap().id
    };
    let s = TepinStore::open(&path).unwrap();
    assert_eq!(s.nodes_in_session("sess-1").unwrap()[0].id, id);
    assert_eq!(s.nodes_of_type("Insight").unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Timing, not a gate (`cargo test -p engram-core history_read_cost -- --ignored --nocapture`):
/// a sealed history-shaped store, 60 sessions × 80 messages, read the old way
/// (every node, filtered) and the indexed way.
#[test]
#[ignore]
pub(super) fn history_read_cost() {
    let e = sealed_capable_engine();
    e.set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    let s = e.store();
    let msg = NodeType::parse("Insight").unwrap();
    for sess in 0..60 {
        let mut head = new_node(NodeType::Anchor, &format!("session {sess}"), "");
        head.session_id = Some(format!("sess-{sess}"));
        s.add_node(head).unwrap();
        for turn in 0..80 {
            let mut n = new_node(
                msg.clone(),
                &format!("turn {turn} of {sess}"),
                &"a reasonably long recorded message body ".repeat(20),
            );
            n.session_id = Some(format!("sess-{sess}"));
            s.add_node(n).unwrap();
        }
    }
    let time = |label: &str, f: &dyn Fn() -> usize| {
        let t = std::time::Instant::now();
        let n = f();
        println!(
            "{label:<44} {:>7.1} ms  ({n} nodes)",
            t.elapsed().as_secs_f64() * 1000.0
        );
    };
    time("list sessions — full scan (old)", &|| {
        s.all_nodes()
            .unwrap()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Anchor)
            .count()
    });
    time("list sessions — indexed by type", &|| {
        s.nodes_of_type("Anchor").unwrap().len()
    });
    time("open one session — full scan (old)", &|| {
        s.all_nodes()
            .unwrap()
            .into_iter()
            .filter(|n| n.session_id.as_deref() == Some("sess-7"))
            .count()
    });
    time("open one session — indexed by session", &|| {
        s.nodes_in_session("sess-7").unwrap().len()
    });
}

/// 0.10.3: the machine core is launched on the home graph, so `open_home`
/// must hand out that same engine. It used to open home.tepin a second time,
/// wait out TepinDB's 3 s lock retry and fail — every project brief silently
/// lost its home section, 3 s late, and `project: "all"` skipped home.
#[test]
fn home_launched_hub_serves_home_from_its_own_engine() {
    let mut env = crate::sync::EnvGuard::lock();
    let tmp = std::env::temp_dir().join(format!("engram-hubhome-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    env.set("ENGRAM_HOME", tmp.join("enghome"));

    let factory: EngineFactory = Box::new(|db: &std::path::Path| {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(Engine::with_store(
            crate::store::open_store(db)?,
            Box::new(FakeEmbedder::default()),
        ))
    });
    let home_db = registry::home_db_path().unwrap();
    std::fs::create_dir_all(home_db.parent().unwrap()).unwrap();
    let home = Engine::with_store(
        crate::store::open_store(&home_db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hub = Hub::new_home(
        std::sync::Arc::new(std::sync::Mutex::new(home)),
        Some(factory),
    );
    hub.current_engine()
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Principle,
            "prefer boring storage engines over clever ones",
            "user-level canon",
        ))
        .unwrap();

    let beta_root = tmp.join("beta");
    std::fs::create_dir_all(&beta_root).unwrap();
    registry::register(&beta_root, &beta_root.join(".engram/graph.tepin")).unwrap();
    hub.get("beta")
        .unwrap()
        .lock()
        .unwrap()
        .add_node(new_node(
            NodeType::Decision,
            "beta stores notes in redb",
            "local",
        ))
        .unwrap();

    assert!(std::sync::Arc::ptr_eq(
        &hub.get("home").unwrap(),
        &hub.current_engine()
    ));

    let started = std::time::Instant::now();
    let brief = hub.brief_for(Some("beta"), 16_000).unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "the brief waited on a store lock: {:?}",
        started.elapsed()
    );
    assert!(
        brief.contains("## Home graph"),
        "home section missing:\n{brief}"
    );
    assert!(brief.contains("prefer boring storage engines"));

    // "all" reads home once (as the current graph), and skips nothing.
    let (hits, skipped) = hub.search_all("boring storage engines", &[], 10).unwrap();
    assert!(skipped.is_empty(), "skipped: {skipped:?}");
    let home_hits = hits
        .iter()
        .filter(|h| h.title.contains("boring storage"))
        .count();
    assert_eq!(home_hits, 1);

    drop(hub);
    env.remove("ENGRAM_HOME");
    let _ = std::fs::remove_dir_all(&tmp);
}
