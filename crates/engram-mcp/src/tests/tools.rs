use crate::*;
use engram_core::{FakeEmbedder, SqliteStore};

pub(crate) fn server() -> Engram {
    Engram::new(Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    ))
}

fn text_of(res: &CallToolResult) -> String {
    format!("{:?}", res.content)
}

pub(crate) fn id_of(r: &CallToolResult) -> String {
    let t = text_of(r);
    let start = t.find("\\\"id\\\": \\\"").unwrap() + 10;
    t[start..].split("\\\"").next().unwrap().to_string()
}

#[tokio::test]
async fn custom_fields_ride_add_note_and_refusals_teach_the_roster() {
    let engine = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let cfg = engram_core::GraphConfig {
        fields: vec![engram_core::config::FieldDef {
            name: "priority".into(),
            label: String::new(),
            kind: engram_core::config::FieldKind::Enum,
            values: vec!["low".into(), "high".into()],
            required: true,
            applies_to: vec!["Decision".into()],
            indexed: false,
            show_in_brief: false,
        }],
        ..engram_core::GraphConfig::default()
    };
    engine.set_graph_config(&cfg).unwrap();
    let s = Engram::new(engine);

    // Refusal without the required field is a teaching error: it names
    // the roster, the vocabulary, and the call shape.
    let err = s
        .add_note(Parameters(note("pick a queue")))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("priority"), "{msg}");
    assert!(msg.contains("low | high"), "{msg}");
    assert!(msg.contains("fields"), "{msg}");

    // The valid write persists and the value comes back on get_node.
    let created = s
        .add_note(Parameters(AddNoteArgs {
            fields: Some(
                [("priority".to_string(), serde_json::json!("high"))]
                    .into_iter()
                    .collect(),
            ),
            ..note("pick a queue")
        }))
        .await
        .unwrap();
    let id = id_of(&created);
    let fetched = s
        .get_node(Parameters(GetNodeArgs {
            id: id.clone(),
            parents: None,
            children: None,
            project: None,
        }))
        .await
        .unwrap();
    assert!(text_of(&fetched).contains("priority"), "field key surfaces");
    assert!(text_of(&fetched).contains("high"), "field value surfaces");

    // update_node merges: null deletes are refused here because the
    // field is required — the teaching error again.
    let err = s
        .update_node(Parameters(UpdateArgs {
            id: id.clone(),
            node_type: None,
            title: None,
            body: None,
            durability: None,
            status: None,
            code_refs: None,
            tags: None,
            version: None,
            fields: Some(
                [("priority".to_string(), serde_json::Value::Null)]
                    .into_iter()
                    .collect(),
            ),
            project: None,
            reviewed: None,
        }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("missing required"), "{err}");
}

#[tokio::test]
async fn add_note_dates_historical_knowledge() {
    let s = server();
    let created = s
        .add_note(Parameters(AddNoteArgs {
            created_at: Some("2025-03-10".into()),
            ..note("a decision recovered from git history")
        }))
        .await
        .unwrap();
    let id = id_of(&created);
    let fetched = s
        .get_node(Parameters(GetNodeArgs {
            id,
            parents: None,
            children: None,
            project: None,
        }))
        .await
        .unwrap();
    // Replies render instants in ISO form (the tidy pass), so the
    // original date is asserted the way an assistant actually reads it.
    assert!(
        text_of(&fetched).contains("2025-03-10T00:00:00Z"),
        "the node carries its original date, not the ingest date"
    );

    let bad = s
        .add_note(Parameters(AddNoteArgs {
            created_at: Some("yesterday".into()),
            ..note("undateable")
        }))
        .await;
    assert!(
        bad.is_err(),
        "an unparseable date is refused, never guessed"
    );
}

pub(crate) fn note(title: &str) -> AddNoteArgs {
    AddNoteArgs {
        version: None,
        created_at: None,
        fields: None,
        node_type: "Decision".into(),
        title: title.into(),
        body: Some("shared body".into()),
        durability: None,
        session_id: None,
        code_refs: vec![],
        tags: vec![],
        project: None,
    }
}

// 0.9.2: tombstones on the read and write paths — a search that returns
// one says so, and a note that re-derives one carries the `tombstoned`
// warning with the tombstone's reason.
#[tokio::test]
async fn search_names_tombstone_hits_and_writes_near_one_warn() {
    let s = server();
    s.add_note(Parameters(AddNoteArgs {
        node_type: "Tombstone".into(),
        body: Some("Deleted Decision. **Why:** the vendor sunset the SDK.".into()),
        ..note("Removed: use the vendor SDK for retries")
    }))
    .await
    .unwrap();

    let body = text_of(
        &s.search(Parameters(SearchArgs {
            query: "vendor SDK retries".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: Some("memory".into()),
            detail: Some("compact".into()),
            ..Default::default()
        }))
        .await
        .unwrap(),
    );
    assert!(
        body.contains("tombstone_note") && body.contains("deliberately removed"),
        "a tombstone hit is named as one: {body}"
    );

    let plain = text_of(
        &s.search(Parameters(SearchArgs {
            query: "nothing like this anywhere".into(),
            types: vec!["Decision".into()],
            limit: None,
            project: None,
            scope: Some("memory".into()),
            ..Default::default()
        }))
        .await
        .unwrap(),
    );
    assert!(
        !plain.contains("tombstone_note"),
        "no tombstone hit, no note: {plain}"
    );

    // A Decision restating the dead knowledge creates (different type)
    // but carries the warning, reason included.
    let verdict = text_of(
        &s.add_note(Parameters(AddNoteArgs {
            body: Some("retries and auth come from the vendor SDK".into()),
            ..note("use the vendor SDK for retries")
        }))
        .await
        .unwrap(),
    );
    // text_of is the Debug rendering, so JSON quotes arrive escaped.
    assert!(verdict.contains("\\\"created\\\": true"), "{verdict}");
    assert!(
        verdict.contains("\\\"reason\\\": \\\"tombstoned\\\"")
            && verdict.contains("vendor sunset the SDK"),
        "tombstoned warning with its note: {verdict}"
    );
    // And no suspect against the tombstone: a `replaces` verdict there
    // would archive the marker.
    assert!(!verdict.contains("\"suspects\""), "{verdict}");
}

#[tokio::test]
async fn add_note_stamps_process_session_id_when_client_omits_it() {
    let s = server();
    let id = id_of(
        &s.add_note(Parameters(note("Adopt SQLite WAL")))
            .await
            .unwrap(),
    );
    let node = s.session_engine().locked().get_node(&id).unwrap().unwrap();
    assert_eq!(node.session_id.as_deref(), Some(&*s.session_id));
    assert!(s.session_id.starts_with("mcp-"));
}

#[tokio::test]
async fn add_note_persists_normalized_tags() {
    let s = server();
    let id = id_of(
        &s.add_note(Parameters(AddNoteArgs {
            created_at: None,
            fields: None,
            tags: vec!["Phase 1".into(), "UI".into()],
            ..note("tagged decision")
        }))
        .await
        .unwrap(),
    );
    let node = s.session_engine().locked().get_node(&id).unwrap().unwrap();
    assert_eq!(node.tags, vec!["phase-1", "ui"]);
}

#[tokio::test]
async fn add_note_short_circuits_duplicates() {
    let s = server();
    let first = s
        .add_note(Parameters(note("Adopt SQLite WAL")))
        .await
        .unwrap();
    assert!(text_of(&first).contains("\\\"created\\\": true"));

    let dupe = s
        .add_note(Parameters(note("Adopt SQLite WAL")))
        .await
        .unwrap();
    let text = text_of(&dupe);
    assert!(text.contains("\\\"created\\\": false"), "got: {text}");
    assert!(text.contains("matched"));
}

#[tokio::test]
async fn get_node_returns_parent_and_child_hierarchy() {
    let s = server();
    // Decision -because-> Principle (parent); Insight -about-> Decision (child).
    let principle = id_of(
        &s.add_note(Parameters(AddNoteArgs {
            version: None,
            created_at: None,
            fields: None,
            node_type: "Principle".into(),
            title: "local first".into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        }))
        .await
        .unwrap(),
    );
    let decision = id_of(
        &s.add_note(Parameters(note("store data in sqlite")))
            .await
            .unwrap(),
    );
    let insight = id_of(
        &s.add_note(Parameters(AddNoteArgs {
            version: None,
            created_at: None,
            fields: None,
            node_type: "Insight".into(),
            title: "wal mode matters".into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        }))
        .await
        .unwrap(),
    );

    s.link(Parameters(LinkArgs {
        from: decision.clone(),
        to: principle.clone(),
        edge_type: "because".into(),
        note: None,
        confidence: None,
        project: None,
    }))
    .await
    .unwrap();
    s.link(Parameters(LinkArgs {
        from: insight.clone(),
        to: decision.clone(),
        edge_type: "about".into(),
        note: None,
        confidence: None,
        project: None,
    }))
    .await
    .unwrap();

    let res = s
        .get_node(Parameters(GetNodeArgs {
            id: decision.clone(),
            parents: Some(2),
            children: Some(2),
            project: None,
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains("parents"), "got: {text}");
    assert!(text.contains("local first"), "parent node inlined: {text}");
    assert!(text.contains("children"), "got: {text}");
    assert!(
        text.contains("wal mode matters"),
        "child node inlined: {text}"
    );
    assert!(
        text.contains("trust"),
        "hierarchy nodes carry trust: {text}"
    );
}

#[tokio::test]
async fn brief_tool_returns_markdown() {
    let s = server();
    s.add_note(Parameters(note("Backend in Rust")))
        .await
        .unwrap();
    let res = s
        .brief(Parameters(BriefArgs {
            max_chars: None,
            project: None,
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains("# Engram brief"));
    assert!(text.contains("Backend in Rust"));
}

/// Tasks over the wire: the JSON shapes the tool schemas
/// promise, the batch `$N` parent, the release auto-close reported by
/// set_version, the done nudge, and the brief section.
#[tokio::test]
async fn task_tools_round_trip() {
    let s = server();
    let json_of = |r: &CallToolResult| -> serde_json::Value {
        let raw = r.content[0].as_text().unwrap().text.clone();
        serde_json::from_str(&raw).unwrap()
    };
    s.set_version(Parameters(
        serde_json::from_value(json!({ "version": "0.9.12" })).unwrap(),
    ))
    .await
    .unwrap();
    let added = s
        .add_tasks(Parameters(
            serde_json::from_value(json!({ "tasks": [
                { "title": "Tasks feature", "body": "- [ ] core\n- [ ] pane" },
                { "title": "Store drivers", "parent": "$1" },
                { "title": "Publish 0.9.12" },
                { "title": "Bad one", "close": "whenever" }
            ]}))
            .unwrap(),
        ))
        .await
        .unwrap();
    let added = json_of(&added);
    let results = added["results"].as_array().unwrap();
    assert_eq!(results[0]["created"], true, "{added}");
    assert_eq!(results[2]["auto_close"], ">0.9.12", "{added}");
    assert!(
        results[3]["error"]
            .as_str()
            .unwrap()
            .contains("can't read the rule")
    );
    let child = results[1]["id"].as_str().unwrap().to_string();

    let listed = json_of(
        &s.list_tasks(Parameters(serde_json::from_value(json!({})).unwrap()))
            .await
            .unwrap(),
    );
    assert_eq!(listed["total"], 3, "{listed}");
    let group = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "Tasks feature")
        .unwrap();
    assert_eq!(group["progress"], json!([0, 1]));

    let brief = text_of(
        &s.brief(Parameters(BriefArgs {
            max_chars: None,
            project: None,
        }))
        .await
        .unwrap(),
    );
    assert!(brief.contains("## Tasks"), "{brief}");
    assert!(brief.contains("— 0/1 done, next: Store drivers"), "{brief}");

    let switched = json_of(
        &s.set_version(Parameters(
            serde_json::from_value(json!({ "version": "0.10.0" })).unwrap(),
        ))
        .await
        .unwrap(),
    );
    assert!(
        switched["tasks"][0]
            .as_str()
            .unwrap()
            .starts_with("Publish 0.9.12 — done:"),
        "{switched}"
    );

    let done = json_of(
        &s.update_task(Parameters(
            serde_json::from_value(json!({
                "id": child, "status": "done", "progress": "both drivers landed"
            }))
            .unwrap(),
        ))
        .await
        .unwrap(),
    );
    assert!(
        done["note"].as_str().unwrap().contains("capture it now"),
        "{done}"
    );
    assert!(
        done["task"]["body"]
            .as_str()
            .unwrap()
            .contains("both drivers landed")
    );
    let closed = json_of(
        &s.list_tasks(Parameters(
            serde_json::from_value(json!({ "status": "closed" })).unwrap(),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(
        closed["total"], 3,
        "group closed with its last task: {closed}"
    );
}

/// A graph with tasks off: the task tools refuse with what happened and
/// what to do instead, as invalid params (the caller's to act on).
#[tokio::test]
async fn task_tools_explain_when_tasks_are_off() {
    let s = server();
    {
        let engine = s.engine_for(&None).unwrap();
        let engine = engine.locked();
        let mut cfg = engine.graph_config();
        cfg.tasks.enabled = false;
        engine.set_graph_config(&cfg).unwrap();
    }
    let err = s
        .add_tasks(Parameters(
            serde_json::from_value(json!({ "tasks": [{ "title": "x" }] })).unwrap(),
        ))
        .await
        .expect_err("tasks are off");
    assert!(
        err.message.contains("turned off") && err.message.contains("handoff"),
        "{}",
        err.message
    );
    let err = s
        .list_tasks(Parameters(serde_json::from_value(json!({})).unwrap()))
        .await
        .expect_err("tasks are off");
    assert!(err.message.contains("turned off"), "{}", err.message);
}

#[tokio::test]
async fn unlink_and_update_edge_roundtrip() {
    let s = server();
    let a = s
        .add_note(Parameters(note("first decision")))
        .await
        .unwrap();
    let b = s
        .add_note(Parameters(note(
            "second decision zzz qqq xyz totally different",
        )))
        .await
        .unwrap();
    let id_of = |r: &CallToolResult| {
        let t = text_of(r);
        let start = t.find("\\\"id\\\": \\\"").unwrap() + 10;
        t[start..].split("\\\"").next().unwrap().to_string()
    };
    let (ia, ib) = (id_of(&a), id_of(&b));

    let linked = s
        .link(Parameters(LinkArgs {
            from: ia,
            to: ib,
            edge_type: "conflicts-with".into(),
            note: None,
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();
    let edge_id = id_of(&linked);

    let upd = s
        .update_edge(Parameters(UpdateEdgeArgs {
            id: edge_id.clone(),
            status: Some("resolved".into()),
            note: None,
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();
    assert!(text_of(&upd).contains("\\\"ok\\\": true"));

    let gone = s
        .unlink(Parameters(IdArg {
            id: edge_id.clone(),
            project: None,
        }))
        .await
        .unwrap();
    assert!(text_of(&gone).contains("\\\"ok\\\": true"));
    assert!(
        s.unlink(Parameters(IdArg {
            id: edge_id,
            project: None,
        }))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn timeline_tool_orders_the_replaces_chain() {
    let s = server();
    let old = id_of(
        &s.add_note(Parameters(AddNoteArgs {
            created_at: None,
            fields: None,
            body: Some("cookie sessions".into()),
            ..note("auth v1")
        }))
        .await
        .unwrap(),
    );
    let new = id_of(
        &s.add_note(Parameters(AddNoteArgs {
            created_at: None,
            fields: None,
            body: Some("oauth device flow".into()),
            ..note("auth v2")
        }))
        .await
        .unwrap(),
    );
    s.link(Parameters(LinkArgs {
        from: new.clone(),
        to: old.clone(),
        edge_type: "replaces".into(),
        note: Some("cookies broke on mobile".into()),
        confidence: None,
        project: None,
    }))
    .await
    .unwrap();

    let t = text_of(
        &s.timeline(Parameters(IdArg {
            id: new,
            project: None,
        }))
        .await
        .unwrap(),
    );
    let (v1, v2) = (t.find("auth v1").unwrap(), t.find("auth v2").unwrap());
    assert!(v1 < v2, "oldest first: {t}");
    assert!(t.contains("cookies broke on mobile"));
    assert!(
        s.timeline(Parameters(IdArg {
            id: "nope".into(),
            project: None,
        }))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn list_drift_flags_missing_refs() {
    let s = server();
    s.add_note(Parameters(AddNoteArgs {
        created_at: None,
        fields: None,
        code_refs: vec!["Cargo.toml".into(), "src/vanished.rs".into()],
        ..note("refs moved")
    }))
    .await
    .unwrap();
    let t = text_of(
        &s.list_drift(Parameters(ProjectArg { project: None }))
            .await
            .unwrap(),
    );
    assert!(t.contains("src/vanished.rs"), "got: {t}");
    assert!(
        !t.contains("Cargo.toml"),
        "existing refs are not drift: {t}"
    );
}

#[tokio::test]
async fn audit_tool_pages_the_journal() {
    let s = server();
    let id = id_of(&s.add_note(Parameters(note("journaled"))).await.unwrap());
    let t = text_of(
        &s.audit(Parameters(AuditArgs {
            limit: None,
            before: None,
            entity_id: Some(id),
            project: None,
        }))
        .await
        .unwrap(),
    );
    assert!(t.contains("created"), "got: {t}");
    assert!(t.contains("journaled"));
}

#[tokio::test]
async fn bulk_create_read_update_roundtrip() {
    let s = server();
    // Batch create: a Decision with a long body, a tagged Caution, and a
    // near-duplicate of the first — the dupe check must run per item.
    let created = s
        .add_notes(Parameters(AddNotesArgs {
            notes: vec![
                AddNoteArgs {
                    created_at: None,
                    fields: None,
                    body: Some("the full body that an export must not lose".into()),
                    ..note("store data in sqlite")
                },
                AddNoteArgs {
                    created_at: None,
                    fields: None,
                    node_type: "Caution".into(),
                    tags: vec!["hygiene".into()],
                    ..note("never trust a relative db path")
                },
                AddNoteArgs {
                    created_at: None,
                    fields: None,
                    body: Some("the full body that an export must not lose".into()),
                    ..note("store data in sqlite")
                },
            ],
        }))
        .await
        .unwrap();
    let text = text_of(&created);
    assert!(text.contains("created"), "got: {text}");
    assert!(text.contains("matched"), "per-item dupe check: {text}");

    // Full-fidelity filtered read: only Decisions, whole body included.
    let listed = s
        .list_nodes(Parameters(ListNodesArgs {
            sort: None,
            types: vec!["Decision".into()],
            status: None,
            tag: None,
            include_archived: None,
            pinned: None,
            limit: None,
            offset: None,
            project: None,
            view: None,
            around: None,
            include_reviewed: None,
        }))
        .await
        .unwrap();
    let text = text_of(&listed);
    assert!(
        text.contains("the full body that an export must not lose"),
        "full body survives the bulk read: {text}"
    );
    assert!(
        !text.contains("relative db path"),
        "type filter holds: {text}"
    );

    // Tag filter reaches the Caution.
    let tagged = s
        .list_nodes(Parameters(ListNodesArgs {
            sort: None,
            types: vec![],
            status: None,
            tag: Some("hygiene".into()),
            include_archived: None,
            pinned: None,
            limit: None,
            offset: None,
            project: None,
            view: None,
            around: None,
            include_reviewed: None,
        }))
        .await
        .unwrap();
    let text = text_of(&tagged);
    assert!(text.contains("relative db path"), "got: {text}");
    assert!(!text.contains("sqlite"), "got: {text}");
}

#[tokio::test]
async fn update_nodes_applies_independently_and_reports_per_item() {
    let s = server();
    let id = id_of(
        &s.add_note(Parameters(note("original title")))
            .await
            .unwrap(),
    );
    let blank = |id: String| UpdateArgs {
        version: None,
        id,
        node_type: None,
        title: None,
        body: None,
        durability: None,
        status: None,
        code_refs: None,
        tags: None,
        fields: None,
        project: None,
        reviewed: None,
    };
    let res = s
        .update_nodes(Parameters(UpdateNodesArgs {
            updates: vec![
                UpdateArgs {
                    title: Some("renamed title".into()),
                    ..blank(id.clone())
                },
                blank("nonexistent-id".into()),
            ],
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(
        text.contains("renamed title") || text.contains("true"),
        "got: {text}"
    );
    assert!(text.contains("false"), "bad id reported, not fatal: {text}");

    let node = s
        .get_node(Parameters(GetNodeArgs {
            id,
            parents: None,
            children: None,
            project: None,
        }))
        .await
        .unwrap();
    assert!(text_of(&node).contains("renamed title"), "patch landed");
}

/// A history-backed server: engine + sibling history store in a temp dir.
fn history_server() -> (Engram, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("engram-mcp-hist-{}", engram_core::id::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.tepin");
    let mut engine = Engine::with_store(
        engram_core::open_store(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    engine.set_history_path(engram_core::history::history_store_path(&db));
    // Recording is opt-in: flip the switch the way the pane does.
    let mut cfg = engine.graph_config();
    cfg.history.enabled = true;
    engine.set_graph_config(&cfg).unwrap();
    (Engram::new(engine), dir)
}

fn seed_history(s: &Engram, sid: &str, texts: &[(&str, &str)]) {
    let engine = s.session_engine();
    let engine = engine.locked();
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for (i, (role, text)) in texts.iter().enumerate() {
        let mut props = serde_json::Map::new();
        props.insert("role".into(), (*role).into());
        props.insert("turn".into(), (i as u64).into());
        engine
            .add_history_node(engram_core::NewNode {
                node_type: engram_core::NodeType::parse("Message").unwrap(),
                title: text.to_string(),
                body: Some(text.to_string()),
                created_at: Some(base - 100 + i as i64),
                durability: engram_core::Durability::Stable,
                source: engram_core::Source::Agent,
                session_id: Some(sid.into()),
                status: None,
                code_refs: vec![],
                tags: vec![],
                version: None,
                props: Some(props),
                fields: None,
            })
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn search_takes_the_temporal_grammar_and_names_what_it_cannot_read() {
    let (s, dir) = history_server();
    seed_history(
        &s,
        "sess-now",
        &[("assistant", "the onnx batch width was the memory culprit")],
    );

    // A relative window the daemon resolves — the assistant never does
    // date arithmetic. The seeded messages are seconds old, so "last week"
    // holds them.
    let res = s
        .search(Parameters(SearchArgs {
            query: "onnx batch width memory".into(),
            scope: Some("history".into()),
            after: Some("last week".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(text_of(&res).contains("sess-now"), "{}", text_of(&res));

    // The same window shifted into the past excludes them.
    let res = s
        .search(Parameters(SearchArgs {
            query: "onnx batch width memory".into(),
            scope: Some("history".into()),
            before: Some("last week".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(!text.contains("sess-now"), "window excludes it: {text}");

    // An unreadable bound is an error naming the offender and teaching the
    // grammar — never a silently dropped filter, which would answer an
    // unscoped question while looking scoped.
    let err = s
        .search(Parameters(SearchArgs {
            query: "anything".into(),
            after: Some("whenever-ish".into()),
            ..Default::default()
        }))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("whenever-ish"), "{err}");
    assert!(err.contains("last week"), "{err}");

    // So is a bad ordering.
    let err = s
        .search(Parameters(SearchArgs {
            query: "anything".into(),
            order: Some("sideways".into()),
            ..Default::default()
        }))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("chronological"), "{err}");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn list_sessions_scopes_to_a_window_and_says_so_when_empty() {
    let (s, dir) = history_server();
    {
        let engine = s.session_engine();
        let engine = engine.locked();
        for (sid, day) in [("sess-june", "2026-06-10"), ("sess-august", "2026-08-10")] {
            let mut props = serde_json::Map::new();
            props.insert("harness".into(), "claude-code".into());
            props.insert("messages".into(), 2u64.into());
            engine
                .add_history_node(engram_core::NewNode {
                    node_type: engram_core::NodeType::parse("Session").unwrap(),
                    title: format!("{sid} opening question"),
                    body: None,
                    created_at: Some(engram_core::parse_day(day).unwrap()),
                    durability: engram_core::Durability::Stable,
                    source: engram_core::Source::Agent,
                    session_id: Some(sid.into()),
                    status: None,
                    code_refs: vec![],
                    tags: vec![],
                    version: None,
                    props: Some(props),
                    fields: None,
                })
                .unwrap()
                .unwrap();
        }
    }

    let listed = |after: Option<&str>, before: Option<&str>| {
        let (after, before) = (after.map(str::to_string), before.map(str::to_string));
        let s = &s;
        async move {
            text_of(
                &s.list_sessions(Parameters(ListSessionsArgs {
                    after,
                    before,
                    ..Default::default()
                }))
                .await
                .unwrap(),
            )
        }
    };

    let text = listed(Some("2026-08-01"), None).await;
    assert!(text.contains("sess-august"), "{text}");
    assert!(!text.contains("sess-june"), "{text}");

    let text = listed(None, Some("2026-08-01")).await;
    assert!(text.contains("sess-june"), "{text}");
    assert!(!text.contains("sess-august"), "{text}");

    // An empty window says the window was empty — not that recording is
    // off, which is a different diagnosis and would send the user to the
    // wrong switch.
    let text = listed(None, Some("2020-01-01")).await;
    assert!(text.contains("No sessions in that window"), "{text}");
    assert!(!text.contains("Nothing recorded"), "{text}");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn search_scope_history_returns_sectioned_hits_and_expands() {
    let (s, dir) = history_server();
    seed_history(
        &s,
        "sess-42",
        &[
            ("user", "why does the daemon leak memory"),
            ("assistant", "the onnx batch width was the memory culprit"),
            ("user", "ship it"),
        ],
    );
    let res = s
        .search(Parameters(SearchArgs {
            detail: None,
            query: "onnx batch width memory".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: Some("history".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains("history"), "{text}");
    assert!(text.contains("sess-42"), "{text}");
    assert!(
        !text.contains("\"confidence\""),
        "history scope skips curated: {text}"
    );

    let res = s
        .expand_history(Parameters(ExpandHistoryArgs {
            session: "sess-42".into(),
            turn: 1,
            window: Some(1),
            project: None,
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains("batch width"), "{text}");
    assert!(text.contains("ship it"), "{text}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn list_sessions_browses_newest_first_with_harness_filter() {
    let (s, dir) = history_server();
    // An empty layer says so instead of returning a bare [].
    let res = s
        .list_sessions(Parameters(ListSessionsArgs {
            limit: None,
            harness: None,
            project: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(text_of(&res).contains("Nothing recorded"));

    {
        let engine = s.session_engine();
        let engine = engine.locked();
        for (sid, harness, ts) in [
            ("sess-old", "claude-code", 1_786_300_000i64),
            ("sess-new", "bob", 1_786_400_000),
        ] {
            let mut props = serde_json::Map::new();
            props.insert("harness".into(), harness.into());
            props.insert("messages".into(), 2u64.into());
            engine
                .add_history_node(engram_core::NewNode {
                    node_type: engram_core::NodeType::parse("Session").unwrap(),
                    title: format!("{sid} opening question"),
                    body: None,
                    created_at: Some(ts),
                    durability: engram_core::Durability::Stable,
                    source: engram_core::Source::Agent,
                    session_id: Some(sid.into()),
                    status: None,
                    code_refs: vec![],
                    tags: vec![],
                    version: None,
                    props: Some(props),
                    fields: None,
                })
                .unwrap()
                .unwrap();
        }
    }
    let res = s
        .list_sessions(Parameters(ListSessionsArgs {
            limit: None,
            harness: None,
            project: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(
        text.contains("sess-new") && text.contains("sess-old"),
        "{text}"
    );
    assert!(
        text.find("sess-new").unwrap() < text.find("sess-old").unwrap(),
        "newest first: {text}"
    );
    let res = s
        .list_sessions(Parameters(ListSessionsArgs {
            limit: None,
            harness: Some("bob".into()),
            project: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(
        text.contains("sess-new") && !text.contains("sess-old"),
        "{text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn get_node_and_expand_history_carry_born_in_provenance() {
    let (s, dir) = history_server();
    // Two recorded turns at fixed times; the note parks at the assistant
    // turn's moment, so provenance resolves in the same pass.
    {
        let engine = s.session_engine();
        let engine = engine.locked();
        for (i, (role, text, ts)) in [
            ("user", "how should we cap the batch", 1_786_400_000i64),
            (
                "assistant",
                "cap it at two — measured, not guessed",
                1_786_400_010,
            ),
        ]
        .iter()
        .enumerate()
        {
            let mut props = serde_json::Map::new();
            props.insert("role".into(), (*role).into());
            props.insert("turn".into(), (i as u64).into());
            engine
                .add_history_node(engram_core::NewNode {
                    node_type: engram_core::NodeType::parse("Message").unwrap(),
                    title: text.to_string(),
                    body: Some(text.to_string()),
                    created_at: Some(*ts),
                    durability: engram_core::Durability::Stable,
                    source: engram_core::Source::Agent,
                    session_id: Some("sess-born".into()),
                    status: None,
                    code_refs: vec![],
                    tags: vec![],
                    version: None,
                    props: Some(props),
                    fields: None,
                })
                .unwrap()
                .unwrap();
        }
    }
    let created = s
        .create_note(AddNoteArgs {
            node_type: "Decision".into(),
            title: "Batch capped at two".into(),
            body: Some("The inference batch stays at two.".into()),
            durability: None,
            tags: vec![],
            code_refs: vec![],
            session_id: None,
            version: None,
            created_at: None,
            fields: None,
            project: None,
        })
        .unwrap();
    let note_id = created["id"].as_str().unwrap().to_string();
    {
        let engine = s.session_engine();
        let engine = engine.locked();
        engine.park_provenance(&note_id, 1_786_400_010);
        assert_eq!(engine.resolve_provenance().unwrap(), 1);
    }
    // get_node carries the born_in line…
    let res = s
        .get_node(Parameters(GetNodeArgs {
            id: note_id.clone(),
            parents: None,
            children: None,
            project: None,
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains("born_in"), "{text}");
    assert!(text.contains("sess-born"), "{text}");
    // …and expand_history the reverse: the notes this session left.
    let res = s
        .expand_history(Parameters(ExpandHistoryArgs {
            session: "sess-born".into(),
            turn: 1,
            window: Some(2),
            project: None,
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains("\\\"notes\\\""), "{text}"); // debug-escaped JSON
    assert!(text.contains(&note_id), "{text}");
    assert!(text.contains("Batch capped at two"), "{text}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn search_auto_falls_through_to_history_only_on_weak_verdicts() {
    let (s, dir) = history_server();
    seed_history(
        &s,
        "sess-7",
        &[("assistant", "we pinned the embedder to fp32")],
    );
    // Empty curated graph: verdict "none" — the section appears.
    let res = s
        .search(Parameters(SearchArgs {
            detail: None,
            query: "embedder pin".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains("history"), "fall-through fired: {text}");
    assert!(text.contains("sess-7"), "{text}");
    // scope=memory never shows history, whatever the verdict.
    let res = s
        .search(Parameters(SearchArgs {
            detail: None,
            query: "embedder pin".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: Some("memory".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(
        !text.contains("sess-7"),
        "memory scope stays curated: {text}"
    );
    // The knob: search_fallthrough=false silences the section.
    {
        let engine = s.session_engine();
        let engine = engine.locked();
        let mut cfg = engine.graph_config();
        cfg.history.search_fallthrough = false;
        engine.set_graph_config(&cfg).unwrap();
    }
    let res = s
        .search(Parameters(SearchArgs {
            detail: None,
            query: "embedder pin".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(
        !text.contains("sess-7"),
        "knob off = no fall-through: {text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn search_snippets_use_brackets_not_sentinels() {
    let s = server();
    s.add_note(Parameters(note("sentinel roundtrip check")))
        .await
        .unwrap();
    let res = s
        .search(Parameters(SearchArgs {
            detail: None,
            query: "sentinel".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.contains('['), "brackets for assistants: {text}");
    assert!(
        !text.contains('\u{e000}') && !text.contains('\u{e001}'),
        "no raw sentinels leak over MCP: {text}"
    );
}
