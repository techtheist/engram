//! Tags and brief overflow.

use super::*;

#[test]
fn reclassification_via_node_patch() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Insight, "actually a decision", ""))
        .unwrap();
    let updated = e
        .update_node(
            &n.id,
            NodePatch {
                node_type: Some(NodeType::Decision),
                ..NodePatch::default()
            },
        )
        .unwrap();
    assert_eq!(updated.node_type, NodeType::Decision);
}

// ---- tags -------------------------------------------------------

#[test]
fn tags_normalize_dedupe_and_roundtrip() {
    let s = store();
    let n = s
        .add_node(NewNode {
            tags: vec![
                "Phase 1".into(),
                "phase-1".into(),
                "  UI  ".into(),
                String::new(),
            ],
            ..new_node(NodeType::Decision, "tagged", "body")
        })
        .unwrap();
    assert_eq!(n.tags, vec!["phase-1", "ui"]);
    assert_eq!(s.get_node(&n.id).unwrap().unwrap().tags, n.tags);
}

#[test]
fn node_patch_replaces_tags_and_none_keeps_them() {
    let s = store();
    let n = s
        .add_node(NewNode {
            tags: vec!["phase-1".into()],
            ..new_node(NodeType::Decision, "tagged", "")
        })
        .unwrap();
    let updated = s
        .update_node(
            &n.id,
            NodePatch {
                tags: Some(vec!["Phase 2".into()]),
                ..NodePatch::default()
            },
        )
        .unwrap();
    assert_eq!(updated.tags, vec!["phase-2"]);
    let untouched = s.update_node(&n.id, NodePatch::default()).unwrap();
    assert_eq!(untouched.tags, vec!["phase-2"]);
}

#[test]
fn tag_stats_count_and_skip_archived() {
    let s = store();
    let a = s
        .add_node(NewNode {
            tags: vec!["alpha".into()],
            ..new_node(NodeType::Decision, "first", "")
        })
        .unwrap();
    let b = s
        .add_node(NewNode {
            tags: vec!["alpha".into(), "beta".into()],
            ..new_node(NodeType::Insight, "second", "")
        })
        .unwrap();

    let stats = s.tag_stats(10).unwrap();
    assert_eq!(stats.len(), 2);
    let alpha = stats.iter().find(|t| t.tag == "alpha").unwrap();
    assert_eq!(alpha.count, 2);

    // Archiving the only carrier of "beta" drops it from the vocabulary.
    s.archive_nodes(std::slice::from_ref(&b.id), now()).unwrap();
    let stats = s.tag_stats(10).unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].tag, "alpha");
    let _ = a;
}

#[test]
fn fts_finds_nodes_by_tag() {
    let s = store();
    let n = s
        .add_node(NewNode {
            tags: vec!["observability".into()],
            ..new_node(NodeType::Decision, "storage layer", "sqlite wal")
        })
        .unwrap();
    let hits = s.search_fts("observability", &[], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, n.id);
}

#[test]
fn update_edge_retypes() {
    let s = store();
    let a = s.add_node(new_node(NodeType::Insight, "a", "")).unwrap();
    let b = s.add_node(new_node(NodeType::Decision, "b", "")).unwrap();
    let e = s
        .add_edge(NewEdge {
            edge_type: EdgeType::About,
            from_id: a.id.clone(),
            to_id: b.id.clone(),
            source: Source::User,
            note: None,
            confidence: None,
            strength: None,
            status: None,
        })
        .unwrap();
    let patched = s
        .update_edge(
            &e.id,
            EdgePatch {
                edge_type: Some(EdgeType::Because),
                ..EdgePatch::default()
            },
        )
        .unwrap();
    assert_eq!(patched.edge_type, EdgeType::Because);
}

#[test]
fn legacy_db_gains_tags_column_and_fts_rebuild() {
    let path = std::env::temp_dir().join(format!("engram-test-{}.db", id::new_id()));

    // A database from before tags existed: no tags column, two-column FTS.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
               id TEXT PRIMARY KEY, type TEXT NOT NULL, title TEXT NOT NULL, body TEXT,
               durability TEXT NOT NULL, source TEXT NOT NULL, session_id TEXT,
               created_at INTEGER NOT NULL, valid_from INTEGER, valid_until INTEGER,
               status TEXT, code_refs TEXT, last_seen INTEGER, approved_at INTEGER
             );
             CREATE VIRTUAL TABLE nodes_fts
               USING fts5(title, body, content='nodes', content_rowid='rowid');
             CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
               INSERT INTO nodes_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
             END;
             INSERT INTO nodes (id, type, title, body, durability, source, created_at, code_refs)
               VALUES ('aaaaaaaaaaaa', 'Decision', 'legacy node', 'old body', 'stable', 'user',
                       1, '[]');",
        )
        .unwrap();
    }

    let s = SqliteStore::open(&path).unwrap();
    let legacy = s.get_node("aaaaaaaaaaaa").unwrap().unwrap();
    assert!(legacy.tags.is_empty(), "pre-tags rows read as untagged");

    // The rebuilt FTS + triggers index tags on the migrated database.
    s.update_node(
        "aaaaaaaaaaaa",
        NodePatch {
            tags: Some(vec!["migrated".into()]),
            ..NodePatch::default()
        },
    )
    .unwrap();
    let hits = s.search_fts("migrated", &[], 10).unwrap();
    assert_eq!(hits.len(), 1, "tag is FTS-searchable after migration");

    drop(s);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn brief_leads_with_recent_tags() {
    let e = engine();
    e.add_node(NewNode {
        tags: vec!["phase-1".into()],
        ..new_node(NodeType::Decision, "tagged decision", "")
    })
    .unwrap();
    let brief = e.brief(2000).unwrap();
    let idx = brief.find("Recent tags").expect("brief names recent tags");
    assert!(
        idx < 100,
        "tags line sits at the top, before budget-cut sections: {brief}"
    );
    assert!(brief.contains("phase-1"));
}

#[test]
fn brief_caps_open_worklist_with_exact_overflow() {
    let e = engine();
    for i in 0..24 {
        e.add_node(NewNode {
            status: Some(NodeStatus::Open),
            durability: Durability::Volatile,
            ..new_node(NodeType::Intent, &format!("todo item number {i}"), "")
        })
        .unwrap();
    }
    let brief = e.brief(50_000).unwrap();
    // "Recently added" comes first and claims the 7 newest; the worklist
    // shows its cap of 10 from the rest and counts the overflow exactly.
    let recent = brief.find("## Recently added").expect("recent section");
    let open = brief.find("## Open problems & intents").expect("worklist");
    assert!(recent < open, "recent must precede the worklist: {brief}");
    assert_eq!(brief.matches("- todo item number").count(), 17);
    assert!(
        brief.contains("…and 7 more — `list_open` has the full worklist."),
        "got: {brief}"
    );
}

#[test]
fn brief_counts_canon_overflow() {
    let e = engine();
    for i in 0..15 {
        e.add_node(new_node(
            NodeType::Decision,
            &format!("decision number {i}"),
            "",
        ))
        .unwrap();
    }
    let brief = e.brief(50_000).unwrap();
    // 7 newest land in "Recently added"; the Decisions section fills its cap
    // of 7 with the others and the overflow counts only what's truly unseen.
    assert_eq!(brief.matches("- decision number").count(), 14);
    assert!(
        brief.contains("…1 more Decisions — `search` reaches them."),
        "got: {brief}"
    );
}

#[test]
fn brief_line_excerpts_stay_compact() {
    let e = engine();
    let long_body = "word ".repeat(120); // ~600 chars, must be cut
    e.add_node(new_node(NodeType::Decision, "verbose decision", &long_body))
        .unwrap();
    e.add_node(new_node(NodeType::Caution, "verbose caution", &long_body))
        .unwrap();
    // enough newer nodes that both fall out of "Recently added" and render
    // in their canon sections
    for i in 0..7 {
        e.add_node(new_node(NodeType::Insight, &format!("filler {i}"), ""))
            .unwrap();
    }
    let brief = e.brief(50_000).unwrap();
    let line = |title: &str| {
        let prefix = format!("- {title}");
        brief
            .lines()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("line for {title} present: {brief}"))
            .to_string()
    };
    let caution = line("verbose caution");
    assert!(
        caution.chars().count() < 210,
        "title + id + ~140-char excerpt, got {} chars: {caution}",
        caution.chars().count()
    );
    assert!(caution.ends_with('…'), "cut excerpts end with an ellipsis");
    let decision = line("verbose decision");
    assert!(
        decision.chars().count() < 150,
        "decisions preview less body (~80 chars), got {} chars: {decision}",
        decision.chars().count()
    );
    assert!(
        decision.chars().count() < caution.chars().count(),
        "decision excerpts are shorter than the default"
    );
}
