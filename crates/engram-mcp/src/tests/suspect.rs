use crate::*;
use engram_core::{FakeEmbedder, SqliteStore};

#[tokio::test]
async fn brief_lists_suspects_and_resolve_judges_them() {
    let s = Engram::new(Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    ));
    let mk = |t: &str, ty: &str| AddNoteArgs {
        version: None,
        created_at: None,
        fields: None,
        node_type: ty.into(),
        title: t.into(),
        body: None,
        durability: None,
        session_id: None,
        code_refs: vec![],
        tags: vec![],
        project: None,
    };
    s.add_note(Parameters(mk("cache invalidation via ttl", "Decision")))
        .await
        .unwrap();
    // Cross-type twin: dodges the duplicate short-circuit, lands as a suspect.
    s.add_note(Parameters(mk("cache invalidation via ttl", "Caution")))
        .await
        .unwrap();

    let listed = format!(
        "{:?}",
        s.list_suspects(Parameters(ProjectArg { project: None }))
            .await
            .unwrap()
            .content
    );
    assert!(listed.contains("suspects"), "got: {listed}");
    let brief = s
        .brief(Parameters(BriefArgs {
            max_chars: None,
            project: None,
        }))
        .await
        .unwrap();
    let brief_text = format!("{:?}", brief.content);
    assert!(
        brief_text.contains("Suspected conflicts"),
        "got: {brief_text}"
    );

    let sid = s.session_engine().locked().suspects().unwrap().remove(0).id;
    let resolved = s
        .resolve_suspect(Parameters(ResolveSuspectArgs {
            id: sid,
            verdict: "conflict".into(),
            project: None,
        }))
        .await
        .unwrap();
    let text = format!("{:?}", resolved.content);
    assert!(text.contains("conflicts-with"), "got: {text}");
}

#[tokio::test]
async fn write_response_surfaces_freshly_queued_suspects() {
    let s = Engram::new(Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    ));
    let mk = |t: &str, ty: &str| AddNoteArgs {
        version: None,
        created_at: None,
        fields: None,
        node_type: ty.into(),
        title: t.into(),
        body: None,
        durability: None,
        session_id: None,
        code_refs: vec![],
        tags: vec![],
        project: None,
    };
    let first = s
        .add_note(Parameters(mk(
            "retry queue drains on reconnect",
            "Decision",
        )))
        .await
        .unwrap();
    let first_text = format!("{:?}", first.content);
    assert!(
        !first_text.contains("suspects"),
        "nothing to suspect yet: {first_text}"
    );

    // Cross-type twin: dodges the duplicate short-circuit, queues a
    // suspect — which the WRITE RESPONSE itself must now surface.
    let second = s
        .add_note(Parameters(mk("retry queue drains on reconnect", "Caution")))
        .await
        .unwrap();
    let text = format!("{:?}", second.content);
    assert!(text.contains("suspects"), "got: {text}");
    assert!(
        text.contains("action_required") && text.contains("resolve_suspect"),
        "the response tells the writer what to do: {text}"
    );
}
