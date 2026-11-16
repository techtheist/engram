//! Hybrid search, reranking and vector retrieval.

use super::*;

pub(super) fn engine() -> Engine {
    Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    )
}

/// Deterministic stand-in for the precision layer: strongly prefers any
/// document containing "WINNER", scoring the rest at a large negative logit.
pub(super) struct FavorWinner;

impl crate::rag::Reranker for FavorWinner {
    fn rank(&self, _query: &str, documents: &[String]) -> crate::Result<Vec<f32>> {
        Ok(documents
            .iter()
            .map(|d| if d.contains("WINNER") { 8.0 } else { -8.0 })
            .collect())
    }
}

#[test]
fn reranker_reorders_hits_and_touches_only_what_is_returned() {
    let mut e = engine();
    e.set_reranker(Box::new(FavorWinner));
    assert!(e.has_reranker());
    // The deciding mode: the cross-encoder's verdict replaces the fused score,
    // so it can promote a candidate from anywhere in the pool.
    let mut cfg = e.graph_config();
    cfg.policy.rerank_vote_k = None;
    e.set_graph_config(&cfg).unwrap();

    let filler_a = e
        .add_node(new_node(NodeType::Insight, "keyword filler alpha", ""))
        .unwrap();
    let filler_b = e
        .add_node(new_node(NodeType::Insight, "keyword filler beta", ""))
        .unwrap();
    let winner = e
        .add_node(new_node(NodeType::Insight, "keyword WINNER gamma", ""))
        .unwrap();

    let hits = e.search("keyword", &[], 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, winner.id, "cross-encoder verdict wins the tie");
    assert!(hits[0].score > 0.0);

    // Over-fetched candidates the reranker discarded are not "seen" — the
    // observability stamp covers only what the caller actually received.
    assert!(e.get_node(&winner.id).unwrap().unwrap().last_seen.is_some());
    for id in [&filler_a.id, &filler_b.id] {
        assert!(e.get_node(id).unwrap().unwrap().last_seen.is_none());
    }
}

/// Scores every document at a large negative logit except the LAST one, which
/// it loves — a reranker whose opinion is exactly opposed to retrieval order.
pub(super) struct FavorLast;

impl crate::rag::Reranker for FavorLast {
    fn rank(&self, _query: &str, documents: &[String]) -> crate::Result<Vec<f32>> {
        let n = documents.len();
        Ok((0..n)
            .map(|i| if i + 1 == n { 8.0 } else { -8.0 })
            .collect())
    }
}

#[test]
fn a_voting_reranker_cannot_bury_what_retrieval_ranked_first() {
    // The contract the default `rerank_vote_k` buys, and the reason it is the
    // default: under a reciprocal-rank vote the cross-encoder is one ranker of
    // two, so a single confident verdict against the top retrieval hit is not
    // enough to unseat it. Under the deciding mode it would be — that is the
    // failure mode this guards, and the same "models nominate, they do not
    // decide" rule the rest of the engine follows.
    let mut e = engine();
    e.set_reranker(Box::new(FavorLast));
    assert!(
        e.graph_config().policy.rerank_vote_k.is_some(),
        "voting is the shipped default"
    );
    // This test is about ORDERING; FavorLast's scripted sigmoid(-8) scores
    // would be tail-trimmed by calibrated delivery (floor and knee alike)
    // before the orderings can be compared, so both trims are switched off
    // for the diagnostic.
    let mut cfg = e.graph_config();
    cfg.policy.delivery_floor = 0.0;
    cfg.policy.knee_cliff = None;
    e.set_graph_config(&cfg).unwrap();

    let mut ids = Vec::new();
    for tag in ["alpha", "beta", "gamma", "delta"] {
        ids.push(
            e.add_node(new_node(NodeType::Insight, &format!("keyword {tag}"), ""))
                .unwrap()
                .id,
        );
    }

    // Run the SAME engine both ways: the two orderings are the assertion, and
    // reading either one alone would prove nothing.
    let mut cfg = e.graph_config();
    cfg.policy.rerank_vote_k = None;
    e.set_graph_config(&cfg).unwrap();
    let decides = e.search("keyword", &[], 4).unwrap();
    assert_eq!(decides.len(), 4);
    let favourite = decides[0].id.clone();

    let mut cfg = e.graph_config();
    cfg.policy.rerank_vote_k = Some(10.0);
    e.set_graph_config(&cfg).unwrap();
    let votes = e.search("keyword", &[], 4).unwrap();

    // Deciding: the cross-encoder's pick takes the top slot outright, from the
    // bottom of the retrieval order. Voting: it is promoted but not enthroned.
    let rank = votes.iter().position(|h| h.id == favourite);
    assert_eq!(
        rank,
        Some(1),
        "one hostile verdict must promote, not overturn: {:?}",
        votes.iter().map(|h| &h.title).collect::<Vec<_>>()
    );
    assert_ne!(
        votes[0].id, favourite,
        "retrieval's top hit keeps its place against a single dissenting model"
    );
    // The reported score stays the cross-encoder's calibrated relevance rather
    // than the reciprocal-rank sum, which encodes rank and not confidence.
    let promoted_score = votes[1].score;
    assert!(
        promoted_score > 0.9,
        "score must remain calibrated relevance, got {promoted_score}"
    );
    assert!(ids.len() == 4);
}

#[test]
fn vector_knn_finds_nearest() {
    let s = store();
    let a = s
        .add_node(new_node(NodeType::Decision, "alpha", ""))
        .unwrap();
    let b = s
        .add_node(new_node(NodeType::Decision, "beta", ""))
        .unwrap();
    let e = FakeEmbedder::default();
    s.upsert_embeddings(&a.id, &[e.embed_one("alpha").unwrap()])
        .unwrap();
    s.upsert_embeddings(&b.id, &[e.embed_one("beta").unwrap()])
        .unwrap();

    let q = e.embed_one("alpha").unwrap();
    let hits = s
        .search_hybrid(
            "",
            Some(&q),
            &[],
            2,
            Default::default(),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(
        hits.first().unwrap().id,
        a.id,
        "nearest vector should rank first"
    );
}

#[test]
fn engine_embeds_on_add_and_hybrid_search_works() {
    let e = engine();
    e.add_node(new_node(
        NodeType::Decision,
        "Adopt SQLite WAL mode",
        "concurrent reads",
    ))
    .unwrap();
    e.add_node(new_node(
        NodeType::Insight,
        "Vue Flow renders the graph",
        "the pane",
    ))
    .unwrap();

    // keyword path
    let hits = e.search("sqlite", &[], 5).unwrap();
    assert_eq!(hits[0].title, "Adopt SQLite WAL mode");

    // vector recall: a query with no exact token match still returns candidates
    let hits = e.search("database engine choices", &[], 5).unwrap();
    assert!(
        !hits.is_empty(),
        "vector half should surface candidates without an FTS match"
    );
}

#[test]
fn engine_reembeds_on_text_update() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Decision, "kanban board", ""))
        .unwrap();
    assert_eq!(e.search("kanban", &[], 5).unwrap()[0].id, n.id);

    e.update_node(
        &n.id,
        NodePatch {
            title: Some("scrum board".into()),
            ..Default::default()
        },
    )
    .unwrap();
    // FTS reflects the new title
    let hits = e.search("scrum", &[], 5).unwrap();
    assert_eq!(hits[0].id, n.id);
}

#[test]
fn trust_boost_prefers_user_source() {
    let e = engine();
    let mut claude = new_node(NodeType::Decision, "shared title here", "shared body text");
    claude.source = Source::Agent;
    claude.durability = Durability::Episodic;
    let c = e.add_node(claude).unwrap();

    let mut user = new_node(NodeType::Decision, "shared title here", "shared body text");
    user.source = Source::User;
    user.durability = Durability::Episodic;
    let u = e.add_node(user).unwrap();

    let hits = e.search("shared title", &[], 5).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    let upos = ids.iter().position(|&i| i == u.id).unwrap();
    let cpos = ids.iter().position(|&i| i == c.id).unwrap();
    assert!(
        upos < cpos,
        "user-sourced node should outrank identical claude node"
    );
}

#[test]
fn deleting_node_removes_embedding() {
    let e = engine();
    let n = e
        .add_node(new_node(NodeType::Decision, "temporary", "to be deleted"))
        .unwrap();
    assert!(!e.search("temporary", &[], 5).unwrap().is_empty());
    assert!(e.delete_node(&n.id).unwrap());
    let hits = e.search("temporary", &[], 5).unwrap();
    assert!(hits.iter().all(|h| h.id != n.id));
}
