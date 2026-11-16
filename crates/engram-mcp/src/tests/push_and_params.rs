use crate::tests::tools::{id_of, note, server};
use crate::*;
use engram_core::{FakeEmbedder, SqliteStore};

fn two_sessions() -> (Arc<Hub>, Engram, Engram) {
    let hub = Arc::new(Hub::single(Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    )));
    (
        hub.clone(),
        Engram::with_hub(hub.clone()),
        Engram::with_hub(hub),
    )
}

fn body_of(r: &CallToolResult) -> String {
    format!("{:?}", r.content)
}

#[tokio::test]
async fn judged_conflicts_push_into_other_live_sessions() {
    let (_hub, writer, reader) = two_sessions();
    let a = writer
        .add_note(Parameters(AddNoteArgs {
            created_at: None,
            fields: None,
            ..note("zzz retry policy: exponential backoff")
        }))
        .await
        .unwrap();
    let b = writer
        .add_note(Parameters(AddNoteArgs {
            created_at: None,
            fields: None,
            ..note("qqq retry policy: never retry anything")
        }))
        .await
        .unwrap();
    let (a_id, b_id) = (id_of(&a), id_of(&b));

    // Reader's calls are clean before any judgment.
    let calm = reader
        .search(Parameters(SearchArgs {
            detail: None,
            query: "retry policy".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(!body_of(&calm).contains("MEMORY ALERT"));

    // The writer records the contradiction…
    writer
        .link(Parameters(LinkArgs {
            from: b_id,
            to: a_id,
            edge_type: "conflicts-with".into(),
            note: None,
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();

    // …the reader's NEXT tool call carries the push, titles included.
    let alerted = reader
        .search(Parameters(SearchArgs {
            detail: None,
            query: "retry policy".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = body_of(&alerted);
    assert!(
        text.contains("MEMORY ALERT"),
        "push rides the next response"
    );
    assert!(text.contains("qqq retry policy"), "alerts carry titles");

    // Delivered once: the reader's following call is clean again.
    let after = reader
        .brief(Parameters(BriefArgs {
            max_chars: Some(500),
            project: None,
        }))
        .await
        .unwrap();
    assert!(!body_of(&after).contains("MEMORY ALERT"));

    // And the writer never gets its own judgment echoed back.
    let writer_next = writer
        .search(Parameters(SearchArgs {
            detail: None,
            query: "retry policy".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(
        !body_of(&writer_next).contains("MEMORY ALERT"),
        "own-write suppression holds"
    );
}

#[tokio::test]
async fn search_detail_tiers_shape_the_response() {
    let s = server();
    s.add_note(Parameters(AddNoteArgs {
        created_at: None,
        fields: None,
        body: Some("a body with plenty of retrieval detail inside it".into()),
        ..note("progressive disclosure subject")
    }))
    .await
    .unwrap();
    let call = |detail: Option<&str>| {
        let s = &s;
        let detail = detail.map(str::to_string);
        async move {
            body_of(
                &s.search(Parameters(SearchArgs {
                    detail,
                    query: "progressive disclosure".into(),
                    types: vec![],
                    limit: None,
                    project: None,
                    scope: None,
                    ..Default::default()
                }))
                .await
                .unwrap(),
            )
        }
    };
    let compact = call(Some("compact")).await;
    assert!(
        !compact.contains("snippet"),
        "compact strips snippets: {compact}"
    );
    assert!(!compact.contains("neighbors"), "compact strips neighbors");
    assert!(compact.contains("progressive disclosure subject"));
    let full = call(Some("full")).await;
    assert!(
        full.contains("plenty of retrieval detail"),
        "full attaches bodies: {full}"
    );
    let bad = s
        .search(Parameters(SearchArgs {
            detail: Some("verbose".into()),
            query: "x".into(),
            types: vec![],
            limit: None,
            project: None,
            scope: None,
            ..Default::default()
        }))
        .await;
    assert!(bad.is_err(), "unknown detail level is refused");
}

#[tokio::test]
async fn list_nodes_sorts_by_connectivity() {
    let s = server();
    let hubnode = s
        .add_note(Parameters(AddNoteArgs {
            created_at: None,
            fields: None,
            ..note("zzz the well connected hub node")
        }))
        .await
        .unwrap();
    let spoke = s
        .add_note(Parameters(AddNoteArgs {
            created_at: None,
            fields: None,
            ..note("qqq a spoke node with one link")
        }))
        .await
        .unwrap();
    s.add_note(Parameters(AddNoteArgs {
        created_at: None,
        fields: None,
        ..note("xxx an island nobody linked or tagged")
    }))
    .await
    .unwrap();
    s.link(Parameters(LinkArgs {
        from: id_of(&spoke),
        to: id_of(&hubnode),
        edge_type: "builds-on".into(),
        note: None,
        confidence: None,
        project: None,
    }))
    .await
    .unwrap();

    let order = |sort: &str| {
        let s = &s;
        let sort = sort.to_string();
        async move {
            body_of(
                &s.list_nodes(Parameters(ListNodesArgs {
                    sort: Some(sort),
                    types: vec![],
                    status: None,
                    tag: None,
                    include_archived: None,
                    pinned: None,
                    offset: None,
                    limit: None,
                    project: None,
                    view: None,
                    around: None,
                    include_reviewed: None,
                }))
                .await
                .unwrap(),
            )
        }
    };
    let least = order("least-connected").await;
    assert!(
        least.find("island nobody").unwrap() < least.find("well connected hub").unwrap(),
        "least-connected surfaces the islands first"
    );
    let most = order("most-connected").await;
    assert!(
        most.find("well connected hub").unwrap() < most.find("island nobody").unwrap(),
        "most-connected surfaces the hubs first"
    );
}
