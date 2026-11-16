//! The history layer: sibling store, chat ontology, props.

use super::*;

#[test]
fn history_ontology_is_valid_and_chat_shaped() {
    let cfg = crate::history::history_ontology();
    cfg.validate().unwrap();
    assert!(cfg.type_def(crate::history::SESSION_TYPE).is_some());
    assert!(cfg.type_def(crate::history::MESSAGE_TYPE).is_some());
    for verb in [
        crate::history::VERB_IN,
        crate::history::VERB_NEXT,
        crate::history::VERB_BORN_IN,
    ] {
        assert!(cfg.verb_def(verb).is_some(), "missing verb {verb}");
    }
    // Chat types never bind to project versions — the harvester stamps
    // explicitly when it wants to.
    assert!(
        !cfg.type_def(crate::history::MESSAGE_TYPE)
            .unwrap()
            .roles
            .versioned
    );
}

#[test]
fn history_config_defaults_off_and_old_documents_parse() {
    // A stored config with no history section reads as DISABLED — recording
    // is opt-in (sealing touches the OS keychain; the pane offers the
    // switch). The harness toggles default on so enabling is one gesture.
    let cfg: GraphConfig = serde_json::from_str("{}").unwrap();
    assert!(!cfg.history.enabled);
    assert!(cfg.history.harnesses.claude_code && cfg.history.harnesses.antigravity);
    assert!(cfg.history.harnesses.kilo && cfg.history.harnesses.gemini);
    assert!(cfg.history.harnesses.bob);
    assert!(cfg.history.search_fallthrough);
    assert!(cfg.history.exclude_paths.is_empty());
    // An empty exclude entry is a config error, not a silent no-op.
    let mut bad = GraphConfig::default();
    bad.history.exclude_paths.push("  ".into());
    assert!(bad.validate().is_err());
}

/// Recording is opt-in: tests that exercise the history layer flip
/// the switch the way the pane does.
pub(super) fn enable_history(e: &Engine) {
    let mut cfg = e.graph_config();
    cfg.history.enabled = true;
    e.set_graph_config(&cfg).unwrap();
}

pub(super) fn props_roundtrip_on(s: &dyn Store, label: &str) {
    let mut n = new_node(NodeType::parse("Insight").unwrap(), "with props", "");
    let mut bag = serde_json::Map::new();
    bag.insert("role".into(), serde_json::json!("assistant"));
    bag.insert("turn".into(), serde_json::json!(7));
    n.props = Some(bag.clone());
    let written = s.add_node(n).unwrap();
    assert_eq!(written.props.as_ref(), Some(&bag), "{label}: add_node");
    let read = s.get_node(&written.id).unwrap().unwrap();
    assert_eq!(read.props.as_ref(), Some(&bag), "{label}: get_node");
    // Upsert (import path) must carry the bag too.
    s.upsert_node(&read).unwrap();
    let again = s.get_node(&written.id).unwrap().unwrap();
    assert_eq!(again.props, Some(bag), "{label}: upsert_node");
    // A node without a bag stays bagless (no empty-object noise).
    let plain = s
        .add_node(new_node(NodeType::parse("Insight").unwrap(), "plain", ""))
        .unwrap();
    assert_eq!(plain.props, None, "{label}: absent stays absent");
}

#[test]
fn node_props_roundtrip_both_drivers() {
    props_roundtrip_on(&store(), "sqlite");
    props_roundtrip_on(&TepinStore::open_in_memory().unwrap(), "tepindb");
}

#[test]
fn engine_opens_history_store_beside_the_graph_and_toggles_live() {
    let dir = std::env::temp_dir().join(format!("engram-history-{}", id::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.tepin");
    let hist = crate::history::history_store_path(&db);
    assert_eq!(hist, dir.join("history.tepin"));

    let store = TepinStore::open(&db).unwrap();
    let mut e = Engine::new(store, Box::new(FakeEmbedder::default()));
    e.set_history_path(hist.clone());
    // Recording is opt-in: with the default config the sibling store does
    // NOT open — no file appears until the user flips the switch.
    assert!(!e.history_open());
    assert!(!hist.is_file());
    let mut cfg0 = e.graph_config();
    cfg0.history.enabled = true;
    e.set_graph_config(&cfg0).unwrap();
    assert!(e.history_open());
    assert!(hist.is_file());
    let seeded = e
        .with_history(|h| GraphConfig::from_stored(h.graph_config().unwrap().as_deref()))
        .unwrap();
    assert_eq!(seeded.ontology.preset, "history");
    assert!(seeded.type_def(crate::history::SESSION_TYPE).is_some());

    // The pane's toggle: disabling via config write drops the handle without
    // a restart; the file stays (delete is a user gesture). Re-enabling
    // reopens and must NOT re-seed over the existing ontology.
    let mut cfg = e.graph_config();
    cfg.history.enabled = false;
    e.set_graph_config(&cfg).unwrap();
    assert!(!e.history_open());
    assert!(hist.is_file());
    cfg.history.enabled = true;
    e.set_graph_config(&cfg).unwrap();
    assert!(e.history_open());

    // History writes land in the sibling store, invisible to the curated
    // graph's surfaces.
    let sid = e
        .with_history(|h| {
            h.add_node(NewNode {
                node_type: NodeType::parse(crate::history::SESSION_TYPE).unwrap(),
                title: "how do I fix the flaky test?".into(),
                body: None,
                created_at: None,
                durability: Durability::Stable,
                source: Source::User,
                session_id: Some("sess-1".into()),
                status: None,
                code_refs: vec![],
                tags: vec![],
                version: None,
                props: None,
                fields: None,
            })
            .unwrap()
            .id
        })
        .unwrap();
    assert!(
        e.get_node(&sid).unwrap().is_none(),
        "curated store can't see it"
    );
    assert_eq!(
        e.graph().unwrap().0.len(),
        0,
        "pane feed stays history-free"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// harvester: adapter parsing, cursors, end-to-end sweep
// ---------------------------------------------------------------------------
