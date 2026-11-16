//! Shared-engine writes and sweeps that judge with the lock released.

use super::*;

pub(super) fn engine_with_nli() -> Engine {
    let mut e = engine();
    e.set_nli(Box::new(crate::nli::FakeNli));
    e
}

/// A logic model as slow as Laya on CPU: every judge call sleeps.
pub(super) struct SlowNli(std::time::Duration);

impl crate::nli::Nli for SlowNli {
    fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
        std::thread::sleep(self.0);
        crate::nli::FakeNli.judge(pairs)
    }
}

#[test]
fn a_shared_sweep_never_holds_the_engine_for_the_whole_scan() {
    // 0.9.9: the session-boundary pass held the engine lock for the whole
    // conflict scan; with a slow logic model (~60 s) every request on the
    // graph queued behind it and the core's async workers starved. The
    // shared sweep releases the lock per node — another caller gets in
    // within one node's judging, however long the scan runs.
    let mut e = engine();
    for i in 0..15 {
        // Plain add_node skips the write-time scan, so the sweep has the
        // judging to do.
        e.add_node(new_node(
            NodeType::Decision,
            &format!("contra: rule {i} forbids tabs"),
            "law",
        ))
        .unwrap();
        e.add_node(new_node(
            NodeType::Insight,
            &format!("contra: rule {i} forbids tabs!!"),
            "law",
        ))
        .unwrap();
    }
    e.set_nli(Box::new(SlowNli(std::time::Duration::from_millis(10))));
    let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
    let sweeper = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let added = Engine::scan_conflicts_shared(&shared).unwrap();
            (added, t0.elapsed())
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..5 {
        let t0 = std::time::Instant::now();
        let guard = shared.lock().unwrap();
        worst = worst.max(t0.elapsed());
        let _ = guard.store().get_node("nope");
        drop(guard);
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    let (added, took) = sweeper.join().unwrap();
    assert!(
        added >= 15,
        "the sweep judged and queued the pairs: {added}"
    );
    assert!(
        took > std::time::Duration::from_millis(600),
        "the scan is long enough to matter: {took:?}"
    );
    assert!(
        worst < std::time::Duration::from_millis(200),
        "a caller waited {worst:?} for the engine during a {took:?} sweep"
    );
}

#[test]
fn a_shared_checked_write_judges_with_the_engine_released() {
    // 0.9.10: a checked write judged its candidate pairs (duplicate,
    // suspects, canon — up to ~14 NLI calls) while holding the engine lock;
    // with a slow logic model every request on the graph waited seconds
    // behind one write. The shared write gathers its pairs under the lock,
    // judges them off it, and queues the result under a short second lock
    // — and still returns the same same-turn verdict.
    let mut e = engine();
    for i in 0..6 {
        e.add_node(new_node(
            NodeType::Decision,
            &format!("contra: sessions live in redis {i}"),
            "",
        ))
        .unwrap();
    }
    e.set_nli(Box::new(SlowNli(std::time::Duration::from_millis(80))));
    let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
    let writer = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let out = Engine::add_node_checked_shared(
                &shared,
                AuditOrigin::mcp("mcp-writer".into()),
                new_node(NodeType::Insight, "contra: sessions live in redis!", ""),
            )
            .unwrap();
            (out, t0.elapsed())
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(60));
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..6 {
        let t0 = std::time::Instant::now();
        let guard = shared.lock().unwrap();
        worst = worst.max(t0.elapsed());
        let _ = guard.store().get_node("nope");
        drop(guard);
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let (out, took) = writer.join().unwrap();
    let WriteOutcome::Created { suspects, .. } = out else {
        panic!("a different-type near-twin is created, not matched");
    };
    assert!(
        !suspects.is_empty(),
        "the write still returns what it queued, judged"
    );
    assert!(
        suspects
            .iter()
            .all(|s| s.nli_label.as_deref() == Some("contradiction")),
        "the off-lock judgments reached the queue"
    );
    assert!(
        took > std::time::Duration::from_millis(400),
        "the judging is long enough to matter: {took:?}"
    );
    assert!(
        worst < std::time::Duration::from_millis(150),
        "a caller waited {worst:?} for the engine during a {took:?} write"
    );
}

/// FakeEmbedder that counts the texts it embeds, optionally sleeping per
/// call like a real model on CPU — the write-path embedding probe.
pub(super) struct CountingEmbedder {
    inner: FakeEmbedder,
    texts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    delay: std::time::Duration,
}

impl CountingEmbedder {
    fn new(delay_ms: u64) -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let texts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (
            Self {
                inner: FakeEmbedder::default(),
                texts: texts.clone(),
                delay: std::time::Duration::from_millis(delay_ms),
            },
            texts,
        )
    }
}

impl Embedder for CountingEmbedder {
    fn dim(&self) -> usize {
        self.inner.dim()
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.texts
            .fetch_add(texts.len(), std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(self.delay);
        self.inner.embed(texts)
    }
    fn is_fake(&self) -> bool {
        true
    }
}

pub(super) fn counting_engine(
    delay_ms: u64,
) -> (Engine, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let (emb, texts) = CountingEmbedder::new(delay_ms);
    (
        Engine::new(SqliteStore::open_in_memory().unwrap(), Box::new(emb)),
        texts,
    )
}

#[test]
fn a_shared_checked_write_embeds_with_the_engine_released() {
    // 0.9.11 (Insight 00dxfvuv1j1f): ~144 of the ~175 ms a checked write
    // held the engine at 1500 notes was embedding — the duplicate check's
    // text, then the stored node's texts. The shared write computes them
    // before it takes the lock, so a reader waits for the store work only.
    let (mut e, _) = counting_engine(0);
    for i in 0..4 {
        e.add_node(new_node(
            NodeType::Decision,
            &format!("sessions live in redis {i}"),
            "",
        ))
        .unwrap();
    }
    let (slow, _) = CountingEmbedder::new(250);
    e.set_embedder(Box::new(slow));
    let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
    let writer = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            Engine::add_node_checked_shared(
                &shared,
                AuditOrigin::mcp("mcp-writer".into()),
                new_node(
                    NodeType::Insight,
                    "Sessions moved to postgres",
                    "The redis box was retired. Sessions now live in a postgres table.",
                ),
            )
            .unwrap();
            t0.elapsed()
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(30));
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..8 {
        let t0 = std::time::Instant::now();
        let guard = shared.lock().unwrap();
        worst = worst.max(t0.elapsed());
        let _ = guard.store().get_node("nope");
        drop(guard);
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let took = writer.join().unwrap();
    assert!(
        took >= std::time::Duration::from_millis(250),
        "the embedding is long enough to matter: {took:?}"
    );
    assert!(
        worst < std::time::Duration::from_millis(150),
        "a reader waited {worst:?} — the write embedded under the lock"
    );
}

#[test]
fn a_shared_write_embeds_each_text_once_and_stores_what_a_plain_write_stores() {
    // The cache may only make a write faster, never different: same stored
    // vector as an unshared write, and the duplicate check's text is not
    // embedded twice when it IS the stored composition.
    let note = || {
        new_node(
            NodeType::Decision,
            "Deploys go through the release train",
            "Every deploy rides the weekly train. Hotfixes are the one exception to it.",
        )
    };
    let (plain, plain_texts) = counting_engine(0);
    let WriteOutcome::Created {
        node: plain_node, ..
    } = plain.add_node_checked(note()).unwrap()
    else {
        panic!("expected a create")
    };
    let (shared, shared_texts) = counting_engine(0);
    let shared = std::sync::Mutex::new(shared);
    let WriteOutcome::Created {
        node: shared_node, ..
    } = Engine::add_node_checked_shared(&shared, AuditOrigin::default(), note()).unwrap()
    else {
        panic!("expected a create")
    };
    let shared = shared.into_inner().unwrap();
    assert_eq!(
        plain.store().embedding_of(&plain_node.id).unwrap(),
        shared.store().embedding_of(&shared_node.id).unwrap(),
        "the cached vector is the vector"
    );
    let plain_n = plain_texts.load(std::sync::atomic::Ordering::Relaxed);
    let shared_n = shared_texts.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        shared_n + 1,
        plain_n,
        "no tags, no fields: the check's text is the stored composition, embedded once"
    );

    // Raw tags read differently from the stored (normalized) ones, so the
    // check embeds its own text — still nothing under the lock twice.
    let (tagged, tagged_texts) = counting_engine(0);
    let tagged = std::sync::Mutex::new(tagged);
    let mut n = note();
    n.tags = vec!["Release Train".into()];
    Engine::add_node_checked_shared(&tagged, AuditOrigin::default(), n).unwrap();
    assert_eq!(
        tagged_texts.load(std::sync::atomic::Ordering::Relaxed),
        plain_n,
        "check text + stored texts, each once"
    );
}

#[test]
fn write_vectors_from_another_model_are_never_stored() {
    // A model swapped between the off-lock embed and the commit: the cache
    // was filled by the old model, so the engine embeds again with its own.
    let e = engine();
    let old = NamedEmbedder {
        model: "old-model",
        width: e.embed_model_id().dim,
    };
    let n = new_node(NodeType::Decision, "Retry budget is 3", "");
    let cache = crate::engine::EmbedCache::for_add(
        &old,
        EmbedModelId {
            name: "old-model".into(),
            dim: old.width,
        },
        &e.graph_config(),
        &n,
    )
    .expect("the fake model embeds");
    let texts = vec!["Retry budget is 3".to_string()];
    let got = e.embed_texts(&texts, Some(&cache)).unwrap();
    assert_eq!(got, FakeEmbedder::default().embed(&texts).unwrap());
    assert_ne!(got, old.embed(&texts).unwrap());
}

#[test]
fn a_shared_update_embeds_off_the_lock_and_matches_a_plain_update() {
    let patch = || NodePatch {
        body: Some("Now three attempts. The old budget of five hid real outages.".into()),
        tags: Some(vec!["Retry Policy".into()]),
        ..Default::default()
    };
    let seed = || new_node(NodeType::Decision, "Retry budget", "Five attempts.");
    let (plain, _) = counting_engine(0);
    let a = plain.add_node(seed()).unwrap();
    plain.update_node_checked(&a.id, patch()).unwrap();

    let (shared, texts) = counting_engine(0);
    let b = shared.add_node(seed()).unwrap();
    let before = texts.load(std::sync::atomic::Ordering::Relaxed);
    let shared = std::sync::Mutex::new(shared);
    Engine::update_node_checked_shared(&shared, AuditOrigin::default(), &b.id, patch()).unwrap();
    let shared = shared.into_inner().unwrap();
    assert_eq!(
        plain.store().embedding_of(&a.id).unwrap(),
        shared.store().embedding_of(&b.id).unwrap()
    );
    let stored = node_texts_len(&shared, &b.id);
    assert_eq!(
        texts.load(std::sync::atomic::Ordering::Relaxed) - before,
        stored,
        "an update's check reads the stored (normalized) text: every text once"
    );
}

/// How many texts a stored node embeds as (composition + claims).
pub(super) fn node_texts_len(e: &Engine, id: &str) -> usize {
    let n = e.store().get_node(id).unwrap().unwrap();
    1 + crate::engine::claim_texts(&n.title, n.body.as_deref()).len()
}

#[test]
fn a_shared_checked_write_skips_pairs_that_changed_while_judged() {
    // The lock is free while the judge runs, so a candidate may be archived
    // in between; the queue step re-checks each pair instead of trusting the
    // snapshot.
    let mut e = engine();
    let older = e
        .add_node(new_node(
            NodeType::Decision,
            "contra: tabs are forbidden",
            "",
        ))
        .unwrap();
    e.set_nli(Box::new(SlowNli(std::time::Duration::from_millis(150))));
    let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
    let writer = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            Engine::add_node_checked_shared(
                &shared,
                AuditOrigin::mcp("mcp-writer".into()),
                new_node(NodeType::Insight, "contra: tabs are forbidden!", ""),
            )
            .unwrap()
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(60));
    shared
        .lock()
        .unwrap()
        .update_node(
            &older.id,
            NodePatch {
                valid_until: Some(crate::store::now()),
                ..NodePatch::default()
            },
        )
        .unwrap();
    let WriteOutcome::Created { suspects, .. } = writer.join().unwrap() else {
        panic!("created");
    };
    assert!(
        suspects.is_empty(),
        "no suspect against a note archived mid-judgment: {suspects:?}"
    );
    assert!(shared.lock().unwrap().suspects().unwrap().is_empty());
}
