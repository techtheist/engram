use crate::*;
use engram_core::{Durability, Embedder, NewNode, Source, SqliteStore};

/// Same-subject texts (`#word`) share an axis; everything else is far.
struct TopicEmbedder;

fn axis(text: &str) -> usize {
    (engram_core::crowding::fnv(&[text.as_bytes()]) % 384) as usize
}

impl Embedder for TopicEmbedder {
    fn dim(&self) -> usize {
        384
    }
    fn name(&self) -> &str {
        "topic-test"
    }
    fn embed(&self, texts: &[String]) -> engram_core::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; 384];
                if let Some(tag) = t.split_whitespace().find(|w| w.starts_with('#')) {
                    v[axis(tag)] += 1.0;
                }
                for salt in ["a", "b", "c"] {
                    v[axis(&format!("{salt}{t}"))] += 0.17;
                }
                v
            })
            .collect())
    }
}

fn server() -> (Engram, Vec<String>) {
    let engine = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(TopicEmbedder),
    );
    let add = |title: String| {
        engine
            .add_node(NewNode {
                node_type: engram_core::NodeType::Decision,
                title,
                body: Some("a body long enough to snip".into()),
                created_at: None,
                durability: Durability::Stable,
                source: Source::Agent,
                session_id: None,
                status: None,
                code_refs: vec![],
                tags: vec![],
                version: None,
                props: None,
                fields: None,
            })
            .unwrap()
            .id
    };
    let pile: Vec<String> = (0..6)
        .map(|i| add(format!("#bridge restatement {i}")))
        .collect();
    for i in 0..30 {
        add(format!("stranger number {i}"));
    }
    (Engram::new(engine), pile)
}

fn json_of(r: &CallToolResult) -> serde_json::Value {
    let raw = r.content[0].as_text().unwrap().text.clone();
    serde_json::from_str(&raw).unwrap()
}

async fn list(s: &Engram, args: serde_json::Value) -> Result<serde_json::Value, ErrorData> {
    let r = s
        .list_nodes(Parameters(serde_json::from_value(args).unwrap()))
        .await?;
    Ok(json_of(&r))
}

#[tokio::test]
async fn conflicts_and_islands_are_sleep_views_too() {
    let (s, pile) = server();
    let conflicts = list(&s, json!({ "view": "conflicts" })).await.unwrap();
    // Empty lists are pruned from replies. Suspects come from the
    // background scan, so their count is timing-dependent.
    assert!(conflicts.get("standing").is_none(), "{conflicts}");

    // Nothing is linked or tagged: every note is an island.
    let page = list(&s, json!({ "view": "islands" })).await.unwrap();
    assert_eq!(page["total"], 36);
    assert_eq!(page["islands"].as_array().unwrap().len(), 20);
    // The pile leads: its notes have the closest neighbours.
    let first = &page["islands"][0];
    assert!(pile.contains(&first["id"].as_str().unwrap().to_string()));
    assert_eq!(first["nearest"].as_array().unwrap().len(), 3);
    assert_eq!(first["nearest"][0]["close"], true);

    let err = list(&s, json!({ "view": "piles" })).await.unwrap_err();
    assert!(err.message.contains("islands"), "{}", err.message);
}

#[tokio::test]
async fn the_review_view_pages_subjects_with_dossiers() {
    let (s, pile) = server();
    let page = list(&s, json!({ "view": "review" })).await.unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["min_size"], 5);
    let subject = &page["subjects"][0];
    assert_eq!(subject["size"], 6);
    assert!(subject["worth"]["score"].as_f64().unwrap() > 0.0);
    let members = subject["members"].as_array().unwrap();
    assert_eq!(members.len(), 6);
    for m in members {
        assert!(pile.contains(&m["id"].as_str().unwrap().to_string()));
        assert_eq!(m["crowding"], 5);
        assert!(m["snippet"].as_str().unwrap().contains("body"));
    }
    assert_eq!(
        subject["pairs"].as_array().unwrap().len(),
        12,
        "15 close pairs, capped"
    );
    assert!(page["band"].as_f64().unwrap() > 0.5);

    let err = list(&s, json!({ "view": "graph" })).await.unwrap_err();
    assert!(err.message.contains("review"));
}

#[tokio::test]
async fn around_and_sort_crowded() {
    let (s, pile) = server();
    let view = list(&s, json!({ "around": pile[0] })).await.unwrap();
    assert_eq!(view["node"]["id"], pile[0]);
    let near: Vec<&str> = view["nearest"]
        .as_array()
        .unwrap()
        .iter()
        .take(5)
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    for p in &pile[1..] {
        assert!(near.contains(&p.as_str()));
    }

    let sorted = list(&s, json!({ "sort": "crowded", "limit": 4 }))
        .await
        .unwrap();
    let nodes = sorted["nodes"].as_array().unwrap();
    assert!(nodes.iter().all(|n| n["crowding"] == 5), "the pile leads");
    assert_eq!(sorted["total"], 36);
}

#[tokio::test]
async fn a_bare_reviewed_mark_is_not_an_update() {
    let (s, pile) = server();
    let before = s
        .mcp(&s.session_engine())
        .get_node(&pile[0])
        .unwrap()
        .unwrap();
    for id in &pile {
        let r = s
            .update_node(Parameters(
                serde_json::from_value(json!({ "id": id, "reviewed": true })).unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(json_of(&r)["reviewed"], true);
    }
    let after = s
        .mcp(&s.session_engine())
        .get_node(&pile[0])
        .unwrap()
        .unwrap();
    assert_eq!(after.confirmed_at, before.confirmed_at, "no trust refresh");
    let page = list(&s, json!({ "view": "review" })).await.unwrap();
    assert_eq!(page["total"], 0);
    assert_eq!(page["hidden_reviewed"], 1);

    // An edit with the mark re-vouches for the new text; one without
    // voids it.
    s.update_nodes(Parameters(
        serde_json::from_value(json!({ "updates": [
            { "id": pile[1], "body": "edited and re-marked", "reviewed": true },
            { "id": pile[2], "body": "edited, mark gone" },
        ]}))
        .unwrap(),
    ))
    .await
    .unwrap();
    // Five of six still marked: the subject stays reviewed.
    let page = list(&s, json!({ "view": "review", "include_reviewed": true }))
        .await
        .unwrap();
    assert_eq!(page["total"], 1);
    let reviewed: Vec<bool> = page["subjects"][0]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["reviewed"].as_bool().unwrap_or(false))
        .collect();
    assert_eq!(reviewed.iter().filter(|r| **r).count(), 5);

    let missing = s
        .update_node(Parameters(
            serde_json::from_value(json!({ "id": "nope", "reviewed": true })).unwrap(),
        ))
        .await;
    assert!(missing.is_err());
}
