#[test]
fn numeric_arguments_accept_numbers_and_numeric_strings() {
    let parse = |v: serde_json::Value| serde_json::from_value::<crate::SearchArgs>(v);
    for limit in [
        serde_json::json!(15),
        serde_json::json!("15"),
        serde_json::json!(" 15 "),
        serde_json::json!(15.0),
    ] {
        let a = parse(serde_json::json!({"query": "q", "limit": limit})).unwrap();
        assert_eq!(a.limit, Some(15));
    }
    let a = parse(serde_json::json!({"query": "q", "limit": ""})).unwrap();
    assert_eq!(a.limit, None, "an empty string reads as absent");
    let a = parse(serde_json::json!({"query": "q", "limit": null})).unwrap();
    assert_eq!(a.limit, None);
    for bad in [
        serde_json::json!("ten"),
        serde_json::json!(-3),
        serde_json::json!(1.5),
    ] {
        let err = parse(serde_json::json!({"query": "q", "limit": bad}))
            .err()
            .expect("refused")
            .to_string();
        assert!(err.contains("non-negative whole number"), "{err}");
    }
    // Required integers and floats take the same path.
    let e: crate::ExpandHistoryArgs =
        serde_json::from_value(serde_json::json!({"session": "s", "turn": "7", "window": "2"}))
            .unwrap();
    assert_eq!((e.turn, e.window), (7, Some(2)));
    let l: crate::LinkArgs = serde_json::from_value(serde_json::json!({
        "from": "a", "to": "b", "type": "because", "confidence": "0.8"
    }))
    .unwrap();
    assert_eq!(l.confidence, Some(0.8));
}

// The advertised schema is unchanged: models are still asked for numbers.
#[test]
fn numeric_arguments_still_advertise_integers() {
    let schema = serde_json::to_value(schemars::schema_for!(crate::SearchArgs)).unwrap();
    let limit = &schema["properties"]["limit"];
    assert!(limit.to_string().contains("integer"), "{limit}");
}
use crate::*;

#[test]
fn file_uris_resolve_to_local_paths_or_nothing() {
    let p = |s: &str| Some(std::path::PathBuf::from(s));
    assert_eq!(file_uri_to_path("file:///Users/x/repo"), p("/Users/x/repo"));
    assert_eq!(file_uri_to_path("file://localhost/x"), p("/x"));
    assert_eq!(file_uri_to_path("file:///a%20b/c"), p("/a b/c"));
    assert_eq!(file_uri_to_path("file://remote-host/x"), None);
    assert_eq!(file_uri_to_path("https://example.com/x"), None);
    assert_eq!(file_uri_to_path("file://"), None);
}

#[test]
fn shipped_ontology_durability_defaults_hold() {
    let cfg = engram_core::config::GraphConfig::default();
    let durability = |name: &str| cfg.type_def(name).unwrap().durability;
    assert_eq!(durability("Decision"), Durability::Stable);
    assert_eq!(durability("Insight"), Durability::Episodic);
    assert_eq!(durability("Intent"), Durability::Volatile);
}

/// Issue #9: rmcp generates tool schemas with draft2020_12 settings,
/// under which a non-inlined item type collapses to a bare
/// `{"$ref": "#/$defs/AddNoteArgs"}` — no field names, no required list —
/// and a small model that doesn't chase the pointer can't shape a batch
/// item. `#[schemars(inline)]` on the item structs is what this locks.
#[test]
fn batch_item_schemas_are_inlined_with_visible_required_fields() {
    let schema_for = |gen_fn: fn(&mut schemars::SchemaGenerator) -> schemars::Schema| {
        let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
        serde_json::to_value(gen_fn(&mut generator)).unwrap()
    };
    let notes = schema_for(schemars::SchemaGenerator::root_schema_for::<AddNotesArgs>);
    let items = &notes["properties"]["notes"]["items"];
    assert!(
        items.get("$ref").is_none(),
        "add_notes items must be inlined, not $ref'd: {items}"
    );
    let required: Vec<&str> = items["required"]
        .as_array()
        .expect("inlined items carry their required list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, ["type", "title"]);
    assert!(
        items["properties"]["type"]["description"]
            .as_str()
            .is_some_and(|d| d.contains("Decision")),
        "the type field must teach its vocabulary in the schema"
    );

    let updates = schema_for(schemars::SchemaGenerator::root_schema_for::<UpdateNodesArgs>);
    assert!(
        updates["properties"]["updates"]["items"]
            .get("$ref")
            .is_none(),
        "update_nodes items must be inlined too"
    );

    let link = schema_for(schemars::SchemaGenerator::root_schema_for::<LinkArgs>);
    assert!(
        link["properties"]["type"]["description"]
            .as_str()
            .is_some_and(|d| d.contains("because")),
        "link's verb field must name its vocabulary in the schema"
    );
}

#[test]
fn type_parsing_is_shape_only() {
    // Ontology-as-data: names parse by shape; whether one
    // exists is the engine's config-driven check, so a custom ontology's
    // types flow through the MCP layer untouched.
    assert!(node_types(&["Decision".into()]).is_ok());
    assert!(node_types(&["Nope".into()]).is_ok());
    assert!(node_types(&[String::new()]).is_err());
    assert!(edge_types(&["because".into()]).is_ok());
    assert!(edge_types(&["relates_to".into()]).is_ok());
    assert!(edge_types(&[String::new()]).is_err());
}

/// 0.9.11: a call that arrives before the first binding waits only the
/// grace, then the bridge answers it itself — with what the startup
/// probe can say — instead of holding it for minutes; a failed binding
/// still answers with its error.
#[tokio::test]
async fn an_unbound_bridge_answers_after_the_grace() {
    let (exit, _exit_rx) = tokio::sync::mpsc::channel(4);
    let (state, _lost_rx) = BridgeState::new(
        reqwest::Client::new(),
        exit,
        Some(Arc::new(|| Some("Downloading local models.".into()))),
        std::time::Duration::from_millis(80),
    );
    let t0 = std::time::Instant::now();
    let peer = state.peer_within_grace().await.unwrap();
    assert!(peer.is_none(), "nothing is bound yet");
    assert!(t0.elapsed() >= std::time::Duration::from_millis(80));
    assert!(t0.elapsed() < std::time::Duration::from_secs(5));
    let note = state.starting_note();
    assert!(note.contains("still starting"), "{note}");
    assert!(note.contains("nothing was written"), "{note}");
    assert!(note.contains("Downloading local models."), "{note}");

    state.fail("engram bridge: no core".into());
    let err = state.peer_within_grace().await.unwrap_err();
    assert!(err.message.contains("no core"), "{err:?}");
}

/// A heartbeat gap reads as sleep only well past the beat: a
/// busy runtime delays a beat by seconds, a closed lid by minutes.
#[test]
fn only_a_long_heartbeat_gap_reads_as_sleep() {
    let beat = std::time::Duration::from_secs(15);
    let secs = std::time::Duration::from_secs;
    assert_eq!(slept_through(secs(15), beat), None);
    assert_eq!(
        slept_through(secs(40), beat),
        None,
        "a slow beat is not sleep"
    );
    assert_eq!(
        slept_through(secs(45), beat),
        None,
        "the boundary is exclusive"
    );
    assert_eq!(slept_through(secs(46), beat), Some(secs(31)));
    assert_eq!(slept_through(secs(3615), beat), Some(secs(3600)));
}

/// The tools a bridge lists before it is bound are the core's own — the
/// same static router — so a client that caches the first list is not
/// misled.
#[test]
fn the_unbound_tool_list_is_the_full_router() {
    let tools = Engram::tool_router().list_all();
    for name in ["brief", "search", "add_note", "describe_ontology"] {
        assert!(tools.iter().any(|t| t.name == name), "missing {name}");
    }
}

/// The silent-death fix (Windsurf field trace): a fatal binding failure
/// must wake every request held in `peer_when_bound` with the real
/// reason, immediately — not let them ride out the 30s cap and die
/// unanswered when the bridge exits.
#[tokio::test]
async fn failed_binding_answers_held_and_future_requests() {
    let (exit, _exit_rx) = tokio::sync::mpsc::channel(4);
    let (state, _lost_rx) = BridgeState::new(reqwest::Client::new(), exit, None, BRIDGE_GRACE);
    // A request already waiting on the binding…
    let held = {
        let state = state.clone();
        tokio::spawn(async move { state.peer_when_bound().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    state.fail("engram bridge: no core is reachable".into());
    // …answers with the failure right away (well under the 30s cap).
    let err = tokio::time::timeout(std::time::Duration::from_secs(2), held)
        .await
        .expect("the held request must wake on failure")
        .unwrap()
        .expect_err("no peer exists — this must be the failure error");
    assert!(
        err.message.contains("no core is reachable"),
        "the held request carries the real reason: {err:?}"
    );
    // A request arriving after the failure errors immediately too.
    let late = tokio::time::timeout(std::time::Duration::from_secs(2), state.peer_when_bound())
        .await
        .expect("a post-failure request must not wait")
        .expect_err("still no peer");
    assert!(late.message.contains("no core is reachable"));
}

#[tokio::test]
async fn add_note_and_search_via_tools() {
    use engram_core::{FakeEmbedder, SqliteStore};
    let engine = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let server = Engram::new(engine);

    let res = server
        .add_note(Parameters(AddNoteArgs {
            version: None,
            created_at: None,
            fields: None,
            node_type: "Decision".into(),
            title: "Adopt SQLite WAL".into(),
            body: Some("concurrent reads".into()),
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        }))
        .await
        .unwrap();
    assert!(!res.is_error.unwrap_or(false));

    let hits = server
        .search(Parameters(SearchArgs {
            detail: None,
            query: "sqlite".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(!hits.is_error.unwrap_or(false));
    // the serialized hit text should mention the node
    let text = format!("{:?}", hits.content);
    assert!(text.contains("Adopt SQLite WAL"));
}
