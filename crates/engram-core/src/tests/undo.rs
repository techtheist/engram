//! Undo from the audit journal.

use super::*;

/// The journal row that recorded `action` on `entity_id`, newest first.
pub(super) fn journal_row(e: &Engine, entity_id: &str, action: &str) -> AuditEntry {
    e.audit_log(None, Some(entity_id), 100)
        .unwrap()
        .entries
        .into_iter()
        .find(|r| r.action == action)
        .unwrap_or_else(|| panic!("no {action} row for {entity_id}"))
}

pub(super) fn as_session(e: &mut Engine, session: &str) {
    e.set_audit_origin(AuditOrigin::mcp(session.to_string()));
}

pub(super) fn in_session(t: NodeType, title: &str, session: &str) -> NewNode {
    let mut n = new_node(t, title, "body");
    n.session_id = Some(session.to_string());
    n
}

#[test]
fn undo_removes_a_created_note_without_a_tombstone_and_marks_the_row() {
    let mut e = engine();
    as_session(&mut e, "mcp-a");
    let n = e
        .add_node(in_session(
            NodeType::Decision,
            "use redis for sessions",
            "mcp-a",
        ))
        .unwrap();
    let created = journal_row(&e, &n.id, "created");

    let dry = e.undo_entry(created.seq, true).unwrap();
    assert!(dry.dry_run && dry.undone.len() == 1, "{dry:?}");
    assert!(
        e.store().get_node(&n.id).unwrap().is_some(),
        "a dry run changes nothing"
    );

    let report = e.undo_entry(created.seq, false).unwrap();
    assert_eq!(report.undone.len(), 1, "{report:?}");
    assert_eq!(report.undone[0].effect, "removes the note");
    assert!(e.store().get_node(&n.id).unwrap().is_none());
    let tombstones = e
        .store()
        .all_nodes()
        .unwrap()
        .into_iter()
        .filter(|x| x.node_type.as_str() == "Tombstone")
        .count();
    assert_eq!(tombstones, 0, "undo is not a hard delete: no tombstone");
    // The original row now reads as undone, by an `undone` row.
    let row = journal_row(&e, &n.id, "created");
    let by = row.undone_by.expect("marked undone");
    let marker = journal_row(&e, &created.seq.to_string(), "undone");
    assert_eq!(marker.seq, by);
    // Undoing it twice is refused as already undone.
    let again = e.undo_entry(created.seq, false).unwrap();
    assert!(again.undone.is_empty() && again.skipped.len() == 1);
}

#[test]
fn undo_restores_an_edit_and_its_search_text() {
    let mut e = engine();
    as_session(&mut e, "mcp-a");
    let n = e
        .add_node(in_session(
            NodeType::Decision,
            "retry budget is three",
            "mcp-a",
        ))
        .unwrap();
    as_session(&mut e, "mcp-b");
    e.update_node(
        &n.id,
        NodePatch {
            title: Some("retry budget is five".into()),
            ..NodePatch::default()
        },
    )
    .unwrap();
    let edit = journal_row(&e, &n.id, "updated");
    e.set_audit_origin(AuditOrigin::pane());
    let report = e.undo_entry(edit.seq, false).unwrap();
    assert_eq!(report.undone.len(), 1, "{report:?}");
    let back = e.store().get_node(&n.id).unwrap().unwrap();
    assert_eq!(back.title, "retry budget is three");
    let hits = e.search("retry budget is three", &[], 5).unwrap();
    assert_eq!(hits.first().map(|h| h.id.as_str()), Some(n.id.as_str()));
}

#[test]
fn session_undo_reverts_its_writes_and_skips_what_changed_since() {
    let mut e = engine();
    as_session(&mut e, "mcp-old");
    let older = e
        .add_node(in_session(
            NodeType::Principle,
            "prefer boring tech",
            "mcp-old",
        ))
        .unwrap();
    // The session under review: two notes, a link, an edit to older canon.
    as_session(&mut e, "mcp-bad");
    let a = e
        .add_node(in_session(
            NodeType::Decision,
            "adopt a graph database",
            "mcp-bad",
        ))
        .unwrap();
    let b = e
        .add_node(in_session(
            NodeType::Insight,
            "graphs are always faster",
            "mcp-bad",
        ))
        .unwrap();
    edge(&e, EdgeType::Because, &a.id, &b.id);
    e.update_node(
        &older.id,
        NodePatch {
            body: Some("rewritten by the bad session".into()),
            ..NodePatch::default()
        },
    )
    .unwrap();
    // Another session edits one of its notes afterwards.
    as_session(&mut e, "mcp-later");
    e.update_node(
        &b.id,
        NodePatch {
            body: Some("reviewed and kept".into()),
            ..NodePatch::default()
        },
    )
    .unwrap();

    e.set_audit_origin(AuditOrigin::pane());
    let report = e.undo_session("mcp-bad", false).unwrap();
    assert!(
        e.store().get_node(&a.id).unwrap().is_none(),
        "its note is removed"
    );
    assert!(
        e.store().get_node(&b.id).unwrap().is_some(),
        "the note another session changed since is kept"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.entity_id == b.id
                && s.note.as_deref().unwrap_or("").contains("changed since")),
        "and the skip says why: {report:?}"
    );
    assert_eq!(
        e.store()
            .get_node(&older.id)
            .unwrap()
            .unwrap()
            .body
            .as_deref(),
        Some("body"),
        "its edit to older canon is reverted"
    );
}

#[test]
fn undoing_a_hard_delete_brings_back_the_note_its_links_and_drops_the_tombstone() {
    let mut e = engine();
    e.set_audit_origin(AuditOrigin::pane());
    let a = e
        .add_node(new_node(NodeType::Decision, "keep this", "body"))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Principle, "because of this", "body"))
        .unwrap();
    let link = edge(&e, EdgeType::Because, &a.id, &b.id);
    let (removed, tombstone) = e
        .delete_node_with_tombstone(&a.id, Some("mistake"), true)
        .unwrap();
    assert!(removed);
    let tombstone = tombstone.expect("the default ontology mints a tombstone");
    assert!(e.store().get_edge(&link.id).unwrap().is_none());

    let deleted = journal_row(&e, &a.id, "deleted");
    assert!(deleted.op_id.is_some(), "a hard delete is one operation");
    let report = e.undo_entry(deleted.seq, false).unwrap();
    assert!(report.skipped.is_empty(), "{report:?}");
    assert!(
        e.store().get_node(&a.id).unwrap().is_some(),
        "the note is back"
    );
    assert!(
        e.store().get_edge(&link.id).unwrap().is_some(),
        "its link is back"
    );
    assert!(
        e.store().get_node(&tombstone.id).unwrap().is_none(),
        "the tombstone the delete minted is gone with it"
    );
}

#[test]
fn an_undo_can_itself_be_undone() {
    let mut e = engine();
    as_session(&mut e, "mcp-a");
    let n = e
        .add_node(in_session(NodeType::Decision, "ship on fridays", "mcp-a"))
        .unwrap();
    let created = journal_row(&e, &n.id, "created");
    e.set_audit_origin(AuditOrigin::pane());
    e.undo_entry(created.seq, false).unwrap();
    assert!(e.store().get_node(&n.id).unwrap().is_none());

    let marker = journal_row(&e, &created.seq.to_string(), "undone");
    let report = e.undo_entry(marker.seq, false).unwrap();
    assert!(!report.undone.is_empty(), "{report:?}");
    assert!(
        e.store().get_node(&n.id).unwrap().is_some(),
        "the note is back"
    );
    assert_eq!(
        journal_row(&e, &n.id, "created").undone_by,
        None,
        "the original row no longer reads as undone"
    );
}

#[test]
fn undoing_a_merge_reverts_the_whole_merge() {
    let mut e = engine();
    e.set_audit_origin(AuditOrigin::pane());
    let survivor = e
        .add_node(new_node(NodeType::Decision, "survivor", "one"))
        .unwrap();
    let victim = e
        .add_node(new_node(NodeType::Decision, "victim", "two"))
        .unwrap();
    let other = e
        .add_node(new_node(NodeType::Principle, "a reason", "three"))
        .unwrap();
    let link = edge(&e, EdgeType::Because, &victim.id, &other.id);
    e.merge_nodes(
        &survivor.id,
        std::slice::from_ref(&victim.id),
        None,
        None,
        Source::User,
    )
    .unwrap();
    assert!(
        e.store()
            .get_node(&victim.id)
            .unwrap()
            .unwrap()
            .valid_until
            .is_some()
    );
    let merged = journal_row(&e, &victim.id, "merged");
    let report = e.undo_entry(merged.seq, false).unwrap();
    assert!(report.undone.len() > 1, "the whole operation: {report:?}");
    let victim_now = e.store().get_node(&victim.id).unwrap().unwrap();
    assert!(victim_now.valid_until.is_none(), "the victim is live again");
    assert_eq!(
        e.store().get_edge(&link.id).unwrap().unwrap().from_id,
        victim.id,
        "its link points at it again"
    );
    let replaces = e
        .store()
        .edges_out(&survivor.id)
        .unwrap()
        .into_iter()
        .filter(|x| x.edge_type.as_str() == "replaces")
        .count();
    assert_eq!(replaces, 0, "the merge's replaces edge is gone");
}
