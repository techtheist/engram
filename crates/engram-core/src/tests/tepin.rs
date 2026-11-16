//! Embedding-model guards and TepinDB at volume.

use super::*;

/// A real-shaped (non-fake) embedder with a chosen identity and width.
pub(super) struct NamedEmbedder {
    pub(super) model: &'static str,
    pub(super) width: usize,
}

impl Embedder for NamedEmbedder {
    fn dim(&self) -> usize {
        self.width
    }
    fn name(&self) -> &str {
        self.model
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.1f32; self.width];
                for &b in t.as_bytes() {
                    v[b as usize % self.width] += 1.0;
                }
                v
            })
            .collect())
    }
}

#[test]
fn embed_model_guard_rebuilds_vectors_on_model_swap() {
    let mut e = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(NamedEmbedder {
            model: "model-a",
            width: 8,
        }),
    );
    // Non-default active model over a virgin store: vector storage reshapes
    // before any write, identity is stamped.
    e.ensure_embed_model().unwrap();
    assert_eq!(e.store().embed_model().unwrap().unwrap().name, "model-a");
    let n = e
        .add_node(new_node(NodeType::Decision, "guarded vectors", ""))
        .unwrap();
    assert_eq!(e.store().embedding_of(&n.id).unwrap().unwrap().len(), 8);

    // Swap the model: the guard rebuilds and re-embeds the whole graph once.
    e.set_embedder(Box::new(NamedEmbedder {
        model: "model-b",
        width: 4,
    }));
    assert_eq!(e.ensure_embed_model().unwrap(), 1);
    let stored = e.store().embed_model().unwrap().unwrap();
    assert_eq!((stored.name.as_str(), stored.dim), ("model-b", 4));
    assert_eq!(e.store().embedding_of(&n.id).unwrap().unwrap().len(), 4);
    assert_eq!(
        e.ensure_embed_model().unwrap(),
        0,
        "idempotent once stamped"
    );
}

#[test]
fn fake_embedder_skips_the_model_guard_entirely() {
    let e = engine();
    e.add_node(new_node(NodeType::Insight, "fake vectors", ""))
        .unwrap();
    assert_eq!(e.ensure_embed_model().unwrap(), 0);
    assert!(
        e.store().embed_model().unwrap().is_none(),
        "a fake open must never stamp or rebuild"
    );
}

#[test]
fn cortex_config_defaults_and_presets() {
    use crate::cortex::{CortexConfig, Role, presets, spec_files};
    let cfg = CortexConfig::default();
    assert_eq!(
        cfg.effective(Role::Embedding).name,
        rag::DEFAULT_EMBED_MODEL
    );
    assert_eq!(cfg.effective(Role::Embedding).dim, Some(384));
    assert_eq!(
        cfg.effective(Role::Reranker).name,
        "jina-reranker-v1-turbo-en"
    );
    assert_eq!(
        cfg.effective(Role::Nli).name,
        "deberta-v3-small-tasksource-nli"
    );
    // NLI needs three files, the fastembed-loaded roles five, a Laya judge
    // four — its graph saved as model.onnx whatever the repo calls it.
    assert_eq!(spec_files(Role::Nli, &presets(Role::Nli)[0]).len(), 3);
    let nli = presets(Role::Nli);
    let names: Vec<&str> = nli.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "deberta-v3-small-tasksource-nli",
            "laya-en-int4",
            "laya-multilingual-int8"
        ]
    );
    assert!(nli.iter().all(|p| p.description.is_some()));
    let laya = spec_files(Role::Nli, &nli[1]);
    assert_eq!(laya.len(), 4);
    assert_eq!(
        laya[0],
        (
            "model.onnx".to_string(),
            "https://huggingface.co/techtheist/laya-onnx/resolve/main/en/model_int4.onnx"
                .to_string()
        )
    );
    assert!(laya.iter().any(|(f, _)| f == "rl_agent_config.json"));
    assert_eq!(
        spec_files(Role::Embedding, &presets(Role::Embedding)[0]).len(),
        5
    );
    // A custom selection round-trips through JSON with the default model_file.
    let spec: crate::cortex::ModelSpec = serde_json::from_str(
        r#"{"name":"my-model","base_url":"https://example.com/m","dim":512,"pooling":"mean"}"#,
    )
    .unwrap();
    assert_eq!(spec.model_file, "onnx/model_quantized.onnx");
    assert_eq!(spec.dim, Some(512));
}

// ---- TepinDB capability checks -----------------------------------------
//
// The db-level behaviors the driver leans on, exercised the way real use
// exercises them: index-served lookups at volume, sequences of writes and
// reads across reopens, counter/meta durability, and serialized multi-thread
// access. The conformance battery proves API parity; these prove the file
// underneath keeps its promises.

#[test]
fn tepin_edge_indexes_answer_correctly_at_volume() {
    let s = TepinStore::open_in_memory().unwrap();
    let nodes: Vec<Node> = (0..40)
        .map(|i| {
            s.add_node(new_node(
                NodeType::Decision,
                &format!("volume node {i}"),
                "",
            ))
            .unwrap()
        })
        .collect();
    // A chain (builds-on) plus a hub (everything about node 0): the shape
    // that makes the from_id/to_id equality indexes earn their keep.
    for i in 1..40 {
        s.add_edge(NewEdge {
            edge_type: EdgeType::BuildsOn,
            from_id: nodes[i].id.clone(),
            to_id: nodes[i - 1].id.clone(),
            source: Source::Agent,
            confidence: None,
            strength: None,
            note: None,
            status: None,
        })
        .unwrap();
        s.add_edge(NewEdge {
            edge_type: EdgeType::About,
            from_id: nodes[i].id.clone(),
            to_id: nodes[0].id.clone(),
            source: Source::Agent,
            confidence: None,
            strength: None,
            note: None,
            status: None,
        })
        .unwrap();
    }
    // Index-served reads must agree exactly with a full scan.
    let all = s.all_edges().unwrap();
    assert_eq!(all.len(), 78);
    for n in [&nodes[0], &nodes[5], &nodes[39]] {
        let scan_in = all.iter().filter(|e| e.to_id == n.id).count();
        let scan_out = all.iter().filter(|e| e.from_id == n.id).count();
        assert_eq!(s.edges_in(&n.id).unwrap().len(), scan_in, "to_id index");
        assert_eq!(s.edges_out(&n.id).unwrap().len(), scan_out, "from_id index");
    }
    assert_eq!(s.edges_in(&nodes[0].id).unwrap().len(), 40); // 39 about + 1 builds-on
    assert_eq!(s.edges_out(&nodes[5].id).unwrap().len(), 2);
    // Traversal over the indexed edges reaches the whole chain.
    let (t_nodes, t_edges) = s.traverse(&nodes[0].id, &[EdgeType::BuildsOn], 39).unwrap();
    assert_eq!(t_nodes.len(), 40);
    assert_eq!(t_edges.len(), 39);
}

#[test]
fn tepin_write_read_sequences_hold_across_reopen() {
    let dir = std::env::temp_dir().join(format!("engram-tepin-seq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("graph.tepin");
    let _ = std::fs::remove_file(&path);

    let (a_id, b_id, suspect_id);
    {
        let s = TepinStore::open(&path).unwrap();
        let a = s
            .add_node(new_node(NodeType::Decision, "alpha decision", "first body"))
            .unwrap();
        let b = s
            .add_node(new_node(NodeType::Principle, "beta principle", ""))
            .unwrap();
        s.add_edge(NewEdge {
            edge_type: EdgeType::Because,
            from_id: a.id.clone(),
            to_id: b.id.clone(),
            source: Source::Agent,
            confidence: None,
            strength: None,
            note: None,
            status: None,
        })
        .unwrap();
        for i in 0..2 {
            s.add_audit(&AuditEntry {
                seq: 0,
                ts: 100 + i,
                action: "created".into(),
                entity: "node".into(),
                entity_id: a.id.clone(),
                title: None,
                before: None,
                after: None,
                origin: "library".into(),
                session_id: None,
                cwd: None,
                pid: None,
                version: None,
                op_id: None,
                undone_by: None,
            })
            .unwrap();
        }
        s.set_embed_version(2).unwrap();
        s.set_embed_model(&EmbedModelId {
            name: "bge-small-en-v1.5".into(),
            dim: 384,
        })
        .unwrap();
        let sus = s.add_suspect(&a.id, &b.id, 0.9, None).unwrap();
        s.set_suspect_status(&sus.id, SuspectStatus::Dismissed)
            .unwrap();
        // A text update re-indexes keywords synchronously, same txn.
        s.update_node(
            &a.id,
            NodePatch {
                title: Some("alpha decision revised".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.search_fts("revised", &[], 5).unwrap().len(), 1);
        (a_id, b_id, suspect_id) = (a.id.clone(), b.id.clone(), sus.id.clone());
    }
    {
        // Reopen 1: every write is there, the audit counter continues.
        let s = TepinStore::open(&path).unwrap();
        assert_eq!(s.all_nodes().unwrap().len(), 2);
        assert_eq!(
            s.get_node(&a_id).unwrap().unwrap().title,
            "alpha decision revised"
        );
        assert_eq!(s.edges_out(&a_id).unwrap().len(), 1);
        assert_eq!(s.embed_version().unwrap(), 2);
        assert_eq!(s.embed_model().unwrap().unwrap().dim, 384);
        assert_eq!(
            s.get_suspect(&suspect_id).unwrap().unwrap().status,
            SuspectStatus::Dismissed
        );
        assert!(
            s.suspect_between(&b_id, &a_id).unwrap(),
            "judgment memory survives"
        );
        assert_eq!(s.search_fts("revised", &[], 5).unwrap().len(), 1);

        let page = s.audit_page(None, None, 10).unwrap();
        assert_eq!(page.total, 2);
        s.add_audit(&AuditEntry {
            seq: 0,
            ts: 300,
            action: "updated".into(),
            entity: "node".into(),
            entity_id: a_id.clone(),
            title: None,
            before: None,
            after: None,
            origin: "library".into(),
            session_id: None,
            cwd: None,
            pid: None,
            version: None,
            op_id: None,
            undone_by: None,
        })
        .unwrap();
        let page = s.audit_page(None, None, 10).unwrap();
        assert_eq!(
            page.entries[0].seq, 3,
            "seq continues after reopen, never reuses"
        );
        assert!(s.delete_node(&a_id).unwrap());
        assert!(s.search_fts("revised", &[], 5).unwrap().is_empty());
    }
    {
        // Reopen 2: the delete and its cascade are durable.
        let s = TepinStore::open(&path).unwrap();
        assert!(s.get_node(&a_id).unwrap().is_none());
        assert!(s.get_node(&b_id).unwrap().is_some());
        assert!(s.edges_in(&b_id).unwrap().is_empty());
        assert_eq!(s.audit_page(None, None, 10).unwrap().total, 3);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tepin_interleaved_updates_keep_search_and_vectors_consistent() {
    // Engine-level (embed + index on every write), mirroring the sqlite
    // re-embed-on-update behavior on the tepin backend.
    let e = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let a = e
        .add_node(new_node(
            NodeType::Decision,
            "cache invalidation policy",
            "ttl",
        ))
        .unwrap();
    let b = e
        .add_node(new_node(NodeType::Insight, "zebra crossing patterns", ""))
        .unwrap();
    assert_eq!(e.search("invalidation", &[], 5).unwrap()[0].id, a.id);

    // Update the text: keyword index and vector must both follow.
    let before = e.store().embedding_of(&a.id).unwrap().unwrap();
    e.update_node(
        &a.id,
        NodePatch {
            title: Some("memoization eviction strategy".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let after = e.store().embedding_of(&a.id).unwrap().unwrap();
    assert_ne!(before, after, "text update re-embeds on tepin");
    // Keyword primitive: the old title's terms are gone from the index
    // (hybrid may still surface the node semantically — that's the vector
    // half doing its job, not staleness).
    assert!(
        e.store()
            .search_fts("invalidation", &[], 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        e.search("memoization eviction", &[], 5).unwrap()[0].id,
        a.id
    );

    // Archive: vector still stored, but no read surface shows the node.
    e.store()
        .archive_nodes(std::slice::from_ref(&a.id), now())
        .unwrap();
    assert!(e.store().embedding_of(&a.id).unwrap().is_some());
    assert!(e.search("memoization eviction", &[], 5).unwrap().is_empty());

    // Delete: the doc takes its vectors with it.
    e.store().delete_node(&b.id).unwrap();
    assert!(e.store().embedding_of(&b.id).unwrap().is_none());
}

#[test]
fn tepin_import_is_idempotent_at_volume() {
    let mk_node = |i: usize| Node {
        id: format!("00volnode{i:03}"),
        node_type: NodeType::Insight,
        title: format!("imported insight number {i}"),
        body: Some(format!("body of note {i}")),
        durability: Durability::Stable,
        source: Source::Agent,
        session_id: Some("bulk".into()),
        created_at: 1_000_000 + i as i64,
        valid_from: Some(1_000_000 + i as i64),
        valid_until: None,
        status: None,
        last_seen: None,
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        trust: 0.0,
        stale: false,
        version: None,
        props: None,
        fields: None,
        code_refs: vec![],
        tags: vec!["bulk".into()],
    };
    let nodes: Vec<Node> = (0..120).map(mk_node).collect();
    let edges: Vec<Edge> = (1..120)
        .map(|i| Edge {
            id: format!("00voledge{i:03}"),
            edge_type: EdgeType::BuildsOn,
            from_id: nodes[i].id.clone(),
            to_id: nodes[i - 1].id.clone(),
            source: Source::Agent,
            created_at: 1_000_000,
            confidence: None,
            strength: None,
            note: None,
            valid_from: None,
            valid_until: None,
            status: None,
        })
        .collect();

    let e = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    for round in 0..2 {
        let summary = e
            .import(ExportGraph {
                config: None,
                version: 1,
                nodes: nodes.clone(),
                edges: edges.clone(),
                tasks: vec![],
            })
            .unwrap();
        assert_eq!((summary.nodes, summary.edges), (120, 119), "round {round}");
        let stats = e.store().stats().unwrap();
        assert_eq!(
            (stats.nodes, stats.edges),
            (120, 119),
            "no dupes on re-import"
        );
        assert_eq!(stats.embedded, 120, "every import round re-embeds all");
    }
    assert_eq!(e.search("imported insight", &[], 5).unwrap().len(), 5);
    assert_eq!(e.store().tag_stats(5).unwrap()[0].count, 120);
}

#[test]
fn tepin_engine_stays_consistent_under_threaded_access() {
    // The daemon serializes every engine behind a mutex; hammer that shape
    // from many threads — interleaved writes, reads, searches — and the
    // counts must come out exact.
    use std::sync::{Arc, Mutex};
    let engine = Arc::new(Mutex::new(Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    )));
    let mut handles = Vec::new();
    for t in 0..8 {
        let engine = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for i in 0..15 {
                let e = engine.lock().unwrap();
                let n = e
                    .add_node(new_node(
                        NodeType::Insight,
                        &format!("thread {t} note {i}"),
                        "",
                    ))
                    .unwrap();
                assert!(!e.search(&format!("thread {t}"), &[], 3).unwrap().is_empty());
                e.store().touch(std::slice::from_ref(&n.id)).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let e = engine.lock().unwrap();
    let stats = e.store().stats().unwrap();
    assert_eq!(stats.nodes, 120);
    assert_eq!(stats.embedded, 120);
    let page = e.store().audit_page(None, None, 1).unwrap();
    assert_eq!(page.total, 120, "one journal row per write, none lost");
}

#[test]
fn tepin_embed_model_swap_keeps_writes_working() {
    // The v0.6.0 live bug: tepin pins each vector write with the store's
    // RECORDED model name, so stamping the new identity after the re-embed
    // loop pinned the file under the old name — and every later write died
    // with embedder_mismatch. The guard now stamps before it re-embeds.
    let mut e = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(NamedEmbedder {
            model: "bge-small-en-v1.5",
            width: 8,
        }),
    );
    e.ensure_embed_model().unwrap();
    e.add_node(new_node(NodeType::Decision, "before the swap", ""))
        .unwrap();

    // The killer case: same width, different name (bge-small → bge-base).
    e.set_embedder(Box::new(NamedEmbedder {
        model: "bge-base-en-v1.5",
        width: 8,
    }));
    assert_eq!(e.ensure_embed_model().unwrap(), 1);
    let after = e
        .add_node(new_node(NodeType::Decision, "after the swap", ""))
        .unwrap();
    assert!(
        e.store().embedding_of(&after.id).unwrap().is_some(),
        "writes keep embedding under the new identity"
    );
    let stored = e.store().embed_model().unwrap().unwrap();
    assert_eq!(stored.name, "bge-base-en-v1.5");

    // And the matching-identity path backfills any vector gaps (mid-swap
    // crash healing): simulate a gap via a raw import (no vectors written).
    let orphan = Node {
        id: "00gaplessnode".into(),
        node_type: NodeType::Insight,
        title: "imported without a vector".into(),
        body: None,
        durability: Durability::Stable,
        source: Source::Agent,
        session_id: None,
        created_at: 1,
        valid_from: Some(1),
        valid_until: None,
        status: None,
        last_seen: None,
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        trust: 0.0,
        stale: false,
        code_refs: vec![],
        tags: vec![],
        version: None,
        props: None,
        fields: None,
    };
    e.store().import_raw(&[orphan], &[]).unwrap();
    assert!(e.store().embedding_of("00gaplessnode").unwrap().is_none());
    assert_eq!(e.ensure_embed_model().unwrap(), 1, "heals exactly the gap");
    assert!(e.store().embedding_of("00gaplessnode").unwrap().is_some());
}

#[test]
fn default_preset_cache_dirs_match_the_legacy_loader_dirs() {
    // The provisioning fix for the default models composes with the legacy
    // loaders ONLY because cortex's cache layout and the pre-selection dirs
    // agree — provision() downloads into the exact dir FastEmbedder::new /
    // FastReranker::new / FastNli::new prefer. Pin that invariant.
    use crate::cortex::{Role, cache_dir, presets};
    assert_eq!(
        cache_dir(&presets(Role::Embedding)[0].name),
        rag::model_dir(),
        "embedding default provisions where the loader looks"
    );
    assert_eq!(
        cache_dir(&presets(Role::Reranker)[0].name),
        rag::reranker_model_dir(),
        "reranker default provisions where the loader looks"
    );
    assert_eq!(
        cache_dir(&presets(Role::Nli)[0].name),
        nli::nli_model_dir(),
        "nli default provisions where the loader looks"
    );
    // And the reranker spec carries the full fastembed five-file layout.
    let files = crate::cortex::spec_files(Role::Reranker, &presets(Role::Reranker)[0]);
    assert_eq!(files.len(), 5);
    assert!(files.iter().all(|(_, url)| url.starts_with("https://")));
}

// ---- historical dating: capture carries its original date ------------------

#[test]
fn parse_day_handles_days_unix_and_rfc3339_prefixes() {
    assert_eq!(parse_day("2026-07-22"), Some(1784678400));
    assert_eq!(parse_day("2024-02-29"), Some(1709164800), "leap day");
    assert_eq!(parse_day("1970-01-01"), Some(0));
    assert_eq!(parse_day("2026-07-22T15:30:00Z"), Some(1784678400));
    assert_eq!(parse_day(" 1784678400 "), Some(1784678400));
    assert_eq!(parse_day("2026-13-01"), None, "month range");
    assert_eq!(parse_day("yesterday"), None);
}
