//! History sealing and graph-store encryption.

use super::*;

#[test]
fn history_sealing_end_to_end() {
    use crate::history::{HistoryKey, is_sealed};
    let mut env = crate::sync::EnvGuard::lock();
    let tmp = std::env::temp_dir().join(format!("engram-seal-{}", id::new_id()));
    let _ = std::fs::remove_dir_all(&tmp);
    env.set("ENGRAM_HOME", tmp.join("enghome"));
    env.set("ENGRAM_KEYRING", "off");

    // Codec: roundtrip, tamper detection, wrong-key refusal.
    let key = HistoryKey::load_or_create().expect("file-fallback key");
    let sealed = key.seal("the batch width was the memory, said the daemon");
    assert!(is_sealed(&sealed));
    assert_eq!(
        key.unseal(&sealed).as_deref(),
        Some("the batch width was the memory, said the daemon")
    );
    // Same key file → same key (a second load reads, not re-mints).
    let again = HistoryKey::load_or_create().unwrap();
    assert!(again.unseal(&sealed).is_some());
    let mut tampered = sealed.clone();
    tampered.replace_range(sealed.len() - 4..sealed.len(), "AAAA");
    assert_eq!(key.unseal(&tampered), None, "AEAD rejects tampering");
    // The redactor's entropy fallback must not eat ciphertext.
    assert_eq!(crate::redact::scrub(&sealed), sealed);

    // Engine: plaintext written pre-seal, sealed by the backlog pass, and
    // every reader still sees prose.
    let dir = tmp.join("proj/.engram");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.tepin");
    let mut e = Engine::with_store(
        crate::store::open_store(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    e.set_history_path(crate::history::history_store_path(&db));
    enable_history(&e);
    let mk = |e: &Engine, text: &str, role: &str, turn: u64| {
        let mut props = serde_json::Map::new();
        props.insert("role".into(), role.into());
        props.insert("turn".into(), turn.into());
        e.add_history_node(NewNode {
            node_type: NodeType::parse("Message").unwrap(),
            title: text.chars().take(40).collect(),
            body: Some(text.into()),
            created_at: Some(now() - 50 + turn as i64),
            durability: Durability::Stable,
            source: Source::Agent,
            session_id: Some("seal-sess".into()),
            status: None,
            code_refs: vec![],
            tags: vec![],
            version: None,
            props: Some(props),
            fields: None,
        })
        .unwrap()
        .unwrap()
    };
    // Pre-encryption era: the store's own recorded state is plaintext.
    let plain_era = mk(&e, "written before the key existed", "user", 0);
    assert_eq!(
        e.with_history(|s| s.encryption_state()).unwrap(),
        crate::seal::EncryptionState::Plaintext
    );

    // 0.9.0: flip the STORE to sealed — the migration reprocesses the
    // backlog, every later write conforms, and readers still see prose
    // because the driver opens sealed rows itself.
    assert!(
        e.set_store_encryption("history", true, &mut |_, _| {})
            .unwrap()
            >= 1
    );
    assert_eq!(
        e.with_history(|s| s.encryption_state()).unwrap(),
        crate::seal::EncryptionState::Sealed
    );
    let sealed_era = mk(&e, "written after sealing turned on", "assistant", 1);
    let read_back = e
        .with_history(|s| s.get_node(&sealed_era.id).unwrap().unwrap())
        .unwrap();
    assert!(
        !is_sealed(&read_back.title) && read_back.title.contains("after sealing"),
        "the driver opens sealed rows for readers: {}",
        read_back.title
    );

    // Readers decrypt in memory: messages, search snippets — and the
    // keyword channel works over the sealed store (blind index).
    let msgs = e.history_messages("seal-sess").unwrap();
    assert_eq!(msgs.len(), 2);
    assert!(msgs.iter().any(|m| m.text.contains("before the key")));
    assert!(msgs.iter().any(|m| m.text.contains("after sealing")));
    let hits = e.search_history("sealing turned on", 5).unwrap();
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| !h.snippet.contains("enc1:")),
        "snippets are prose, not blobs: {hits:?}"
    );
    let kw = e
        .with_history(|s| s.search_fts("sealing turned", &[], 5))
        .unwrap()
        .unwrap();
    assert!(
        kw.iter().any(|h| h.id == sealed_era.id),
        "BM25 finds terms through the blind index: {kw:?}"
    );

    // At rest it is REALLY sealed: a reopen where no key exists renders the
    // placeholder, never plaintext, never garbage.
    let history_db = crate::history::history_store_path(&db);
    drop(e);
    env.set("ENGRAM_HOME", tmp.join("keyless"));
    {
        let s = crate::store::open_store(&history_db).unwrap();
        let node = s.get_node(&plain_era.id).unwrap().unwrap();
        assert!(
            node.title.contains("unavailable"),
            "no key → placeholder: {}",
            node.title
        );
    }

    // Key restored: prose again — then decrypt the whole store and verify
    // the file is readable with no key at all.
    env.set("ENGRAM_HOME", tmp.join("enghome"));
    {
        let mut e = Engine::with_store(
            crate::store::open_store(&db).unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        e.set_history_path(history_db.clone());
        enable_history(&e);
        let back = e
            .with_history(|s| s.get_node(&plain_era.id).unwrap().unwrap())
            .unwrap();
        assert!(back.title.contains("before the key"), "{}", back.title);
        e.set_store_encryption("history", false, &mut |_, _| {})
            .unwrap();
        assert_eq!(
            e.with_history(|s| s.encryption_state()).unwrap(),
            crate::seal::EncryptionState::Plaintext
        );
    }
    env.set("ENGRAM_HOME", tmp.join("keyless2"));
    {
        let s = crate::store::open_store(&history_db).unwrap();
        let node = s.get_node(&sealed_era.id).unwrap().unwrap();
        assert!(
            node.title.contains("after sealing"),
            "decrypted store reads keyless: {}",
            node.title
        );
    }

    env.remove("ENGRAM_KEYRING");
    env.remove("ENGRAM_HOME");
    std::fs::remove_dir_all(&tmp).ok();
}

// ---------------------------------------------------------------------------
// 0.9.0: graph-store encryption — parity, migration, resume.
// ---------------------------------------------------------------------------

/// A tepin engine with an injected ephemeral key — no keyring, no disk.
pub(super) fn sealed_capable_engine() -> Engine {
    let store = TepinStore::open_in_memory().unwrap();
    store.set_seal_key_for_tests(crate::seal::SealKey::ephemeral().unwrap());
    Engine::new(store, Box::new(FakeEmbedder::default()))
}

pub(super) fn seed_corpus(e: &Engine) -> Vec<Node> {
    let rows = [
        (
            "Adopt sqlite for the ledger",
            "write-ahead logging keeps readers unblocked",
        ),
        (
            "The ledger flushes hourly",
            "a cron task drains the sqlite queue into cold storage",
        ),
        (
            "Postgres was rejected",
            "operational burden outweighed the gains for a single node",
        ),
        (
            "Retry with jitter",
            "fixed one-second retries hammered the API during incidents",
        ),
        (
            "The exporter owns backoff",
            "retry policy lives beside the queue, not in callers",
        ),
    ];
    rows.iter()
        .map(|(t, b)| e.add_node(new_node(NodeType::Decision, t, b)).unwrap())
        .collect()
}

#[test]
fn bm25_ranking_is_identical_sealed_and_plaintext() {
    let plain = sealed_capable_engine();
    let sealed = sealed_capable_engine();
    seed_corpus(&plain);
    seed_corpus(&sealed);
    sealed
        .set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    for query in [
        "sqlite ledger",
        "retry jitter backoff",
        "postgres operational burden",
        "hourly flush cron",
        "nothing matches this at all zzz",
    ] {
        let a: Vec<(String, String)> = plain
            .store()
            .search_fts(query, &[], 10)
            .unwrap()
            .into_iter()
            .map(|h| (h.title, format!("{:.9}", h.score)))
            .collect();
        let b: Vec<(String, String)> = sealed
            .store()
            .search_fts(query, &[], 10)
            .unwrap()
            .into_iter()
            .map(|h| (h.title, format!("{:.9}", h.score)))
            .collect();
        assert_eq!(a, b, "BM25 must be bit-identical for {query:?}");
    }
}

#[test]
fn graph_encryption_round_trip_preserves_everything() {
    let e = sealed_capable_engine();
    let nodes = seed_corpus(&e);
    let edge = e
        .add_edge(NewEdge {
            edge_type: EdgeType::Because,
            from_id: nodes[0].id.clone(),
            to_id: nodes[2].id.clone(),
            source: Source::Agent,
            note: Some("the ops-burden analysis".into()),
            confidence: None,
            strength: None,
            status: None,
        })
        .unwrap();

    let before_export = e.export().unwrap();
    let mut seen = 0;
    e.set_store_encryption("graph", true, &mut |done, total| {
        assert!(done <= total);
        seen = done;
    })
    .unwrap();
    assert!(seen > 0, "progress callbacks fire");
    assert_eq!(
        e.store().encryption_state(),
        crate::seal::EncryptionState::Sealed
    );

    // Every read surface still sees prose: nodes, edges, audit, exports.
    let n = e.get_node(&nodes[0].id).unwrap().unwrap();
    assert_eq!(n.title, "Adopt sqlite for the ledger");
    assert_eq!(
        n.body.as_deref(),
        Some("write-ahead logging keeps readers unblocked")
    );
    let edges = e.store().edges_out(&nodes[0].id).unwrap();
    assert_eq!(edges[0].note.as_deref(), Some("the ops-burden analysis"));
    let page = e.store().audit_page(None, None, 100).unwrap();
    assert!(
        page.entries
            .iter()
            .any(|a| a.title.as_deref() == Some("Adopt sqlite for the ledger")),
        "audit rows open for readers"
    );
    let after_export = e.export().unwrap();
    assert_eq!(
        serde_json::json!(before_export.nodes),
        serde_json::json!(after_export.nodes),
        "exports decrypt — a user export is plaintext by intent"
    );

    // Decrypt restores the recorded state and everything still reads.
    e.set_store_encryption("graph", false, &mut |_, _| {})
        .unwrap();
    assert_eq!(
        e.store().encryption_state(),
        crate::seal::EncryptionState::Plaintext
    );
    let n = e.get_node(&nodes[0].id).unwrap().unwrap();
    assert_eq!(n.title, "Adopt sqlite for the ledger");
    let _ = edge;
}

#[test]
fn interrupted_migration_resumes_from_the_recorded_state() {
    let e = sealed_capable_engine();
    seed_corpus(&e);
    // Simulate a crash mid-migration: the state advanced to `sealing` but
    // the rewrite never ran. Readers must already cope, and a reconcile
    // toward the same target must finish the job.
    e.store()
        .set_encryption_state(crate::seal::EncryptionState::Sealing)
        .unwrap();
    let n = e.store().all_nodes().unwrap();
    assert_eq!(n.len(), 5, "mixed rows read fine mid-flight");
    let reprocessed = e
        .set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    assert!(reprocessed >= 5);
    assert_eq!(
        e.store().encryption_state(),
        crate::seal::EncryptionState::Sealed
    );
    // Idempotent: converging again touches nothing.
    assert_eq!(
        e.set_store_encryption("graph", true, &mut |_, _| {})
            .unwrap(),
        0
    );
}

#[test]
fn sealed_graph_search_hybrid_still_delivers() {
    let e = sealed_capable_engine();
    let nodes = seed_corpus(&e);
    e.set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    let hits = e.search("retry jitter during incidents", &[], 5).unwrap();
    assert!(
        hits.iter().any(|h| h.id == nodes[3].id),
        "hybrid search over a sealed store finds the note: {hits:?}"
    );
    assert!(
        hits.iter().all(|h| !h.title.contains("enc1:")),
        "delivered hits are prose"
    );
}

/// Perf receipt probe (run manually: cargo test --release history_codec_receipt -- --ignored --nocapture).
#[test]
#[ignore]
pub(super) fn history_codec_receipt() {
    let mut env = crate::sync::EnvGuard::lock();
    let tmp = std::env::temp_dir().join(format!("engram-perf-{}", id::new_id()));
    env.set("ENGRAM_HOME", &tmp);
    env.set("ENGRAM_KEYRING", "off");
    let key = crate::history::HistoryKey::load_or_create().unwrap();
    // Prose-register payloads around the sizes real turns have.
    let mk = |i: usize, len: usize| {
        let mut s = String::new();
        while s.len() < len {
            s.push_str(&format!(
                "The daemon's batch width turned out to be the memory culprit in run {i}; \
                 we measured the arena, the thread pools and the allocator before believing it. "
            ));
        }
        s
    };
    let bodies: Vec<String> = (0..500).map(|i| mk(i, 900)).collect();
    let raw: usize = bodies.iter().map(String::len).sum();
    let t0 = std::time::Instant::now();
    let sealed: Vec<String> = bodies.iter().map(|b| key.seal(b)).collect();
    let seal_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let stored: usize = sealed.iter().map(String::len).sum();
    let t1 = std::time::Instant::now();
    for s in &sealed {
        key.unseal(s).unwrap();
    }
    let unseal_ms = t1.elapsed().as_secs_f64() * 1000.0;
    println!(
        "codec receipt: 500 bodies x ~900B | raw {raw}B -> stored {stored}B ({:.2}x) | seal {seal_ms:.1}ms total ({:.3}ms each) | unseal {unseal_ms:.1}ms total ({:.3}ms per candidate)",
        stored as f64 / raw as f64,
        seal_ms / 500.0,
        unseal_ms / 500.0
    );
    env.remove("ENGRAM_KEYRING");
    env.remove("ENGRAM_HOME");
    std::fs::remove_dir_all(&tmp).ok();
}
