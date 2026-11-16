use crate::*;
use engram_core::{FakeEmbedder, SqliteStore};

fn server() -> Engram {
    Engram::new(Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    ))
}

#[tokio::test]
async fn all_writes_are_refused_with_the_home_pointer() {
    let s = server();
    let err = s
        .add_note(Parameters(AddNoteArgs {
            version: None,
            created_at: None,
            fields: None,
            node_type: "Decision".into(),
            title: "fan out".into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: Some("all".into()),
        }))
        .await
        .unwrap_err();
    assert!(err.message.contains("home"), "got: {}", err.message);
}

#[tokio::test]
async fn unknown_project_selector_is_invalid_params() {
    let s = server();
    let err = s
        .search(Parameters(SearchArgs {
            detail: None,
            query: "anything".into(),
            types: vec![],
            limit: None,
            project: Some("definitely-not-registered-xyz".into()),
            scope: None,
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("definitely-not-registered-xyz"),
        "got: {}",
        err.message
    );
}

#[tokio::test]
async fn list_projects_reports_the_current_project() {
    let s = server();
    let t = format!("{:?}", s.list_projects().await.unwrap().content);
    assert!(t.contains("projects"), "got: {t}");
    assert!(t.contains("current"), "got: {t}");
}
