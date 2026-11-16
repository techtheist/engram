//! The harvester: adapters, cursors, end-to-end sweeps, provenance.

use super::*;

#[test]
fn iso8601_parses_the_shapes_harnesses_write() {
    let p = crate::harvest::parse_iso8601;
    assert_eq!(p("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(p("2026-08-11T09:15:32Z"), Some(1786439732));
    // Fractional seconds are ignored, not rejected.
    assert_eq!(p("2026-08-11T09:15:32.123Z"), Some(1786439732));
    // Offsets shift back to UTC.
    assert_eq!(p("2026-08-11T11:15:32+02:00"), Some(1786439732));
    assert_eq!(p("2026-08-11T04:15:32.5-05:00"), Some(1786439732));
    assert_eq!(p("not a date"), None);
    assert_eq!(p("2026-13-40T09:15:32Z"), None);
}

#[test]
fn truncate_words_cuts_on_boundaries() {
    let t = crate::harvest::truncate_words;
    assert_eq!(t("short title", 80), "short title");
    assert_eq!(t("  collapse \n whitespace  ", 80), "collapse whitespace");
    let cut = t("one two three four five six seven eight", 15);
    assert_eq!(cut, "one two three…");
    // Multi-byte safety: never panics mid-char.
    let _ = t(&"é".repeat(200), 15);
}

pub(super) fn cc_record(
    kind: &str,
    uuid: &str,
    session: &str,
    cwd: &std::path::Path,
    ts: &str,
    content: serde_json::Value,
    extra: &[(&str, serde_json::Value)],
) -> String {
    let mut v = serde_json::json!({
        "type": kind,
        "uuid": uuid,
        "sessionId": session,
        "cwd": cwd.display().to_string(),
        "timestamp": ts,
        "isSidechain": false,
        "message": { "role": kind, "content": content },
    });
    for (k, val) in extra {
        v[*k] = val.clone();
    }
    v.to_string()
}

#[test]
fn claude_code_adapter_filters_and_cursors() {
    use crate::harvest::{HistoryAdapter, Role, claude_code::ClaudeCodeAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-ccad-{}", id::new_id()));
    let proj = tmp.join("projects").join("-slug");
    std::fs::create_dir_all(&proj).unwrap();
    let cwd = std::path::PathBuf::from("/repo");
    let file = proj.join("11111111-2222-3333-4444-555555555555.jsonl");

    let lines = [
        // Keep: plain user prose (string content).
        cc_record(
            "user",
            "u1",
            "s1",
            &cwd,
            "2026-08-11T10:00:00Z",
            serde_json::json!("how do I fix the flaky test?"),
            &[],
        ),
        // Keep: assistant — text block only survives out of the mix.
        cc_record(
            "assistant",
            "a1",
            "s1",
            &cwd,
            "2026-08-11T10:00:05Z",
            serde_json::json!([
                {"type": "thinking", "thinking": "…"},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}},
                {"type": "text", "text": "Run it with --nocapture."}
            ]),
            &[],
        ),
        // Drop: tool-result echo (role user, machine traffic).
        cc_record(
            "user",
            "u2",
            "s1",
            &cwd,
            "2026-08-11T10:00:06Z",
            serde_json::json!([{"type": "tool_result", "content": "…"}]),
            &[("toolUseResult", serde_json::json!({"stdout": "…"}))],
        ),
        // Drop: sidechain (subagent) traffic.
        cc_record(
            "assistant",
            "a2",
            "s1",
            &cwd,
            "2026-08-11T10:00:07Z",
            serde_json::json!([{"type": "text", "text": "subagent says"}]),
            &[("isSidechain", serde_json::json!(true))],
        ),
        // Drop: synthetic meta record.
        cc_record(
            "user",
            "u3",
            "s1",
            &cwd,
            "2026-08-11T10:00:08Z",
            serde_json::json!("meta"),
            &[("isMeta", serde_json::json!(true))],
        ),
        // Drop: slash-command scaffolding wearing type user (live-backfill find).
        cc_record(
            "user",
            "u5",
            "s1",
            &cwd,
            "2026-08-11T10:00:08Z",
            serde_json::json!(
                "<command-name>/clear</command-name>\n<command-message>clear</command-message>"
            ),
            &[],
        ),
        // Drop: non-conversational record types entirely.
        r#"{"type":"file-history-snapshot","messageId":"x"}"#.to_string(),
        r#"{"type":"system","content":"…"}"#.to_string(),
    ];
    let mut body = lines.join("\n");
    body.push('\n');
    // A partial trailing line must hold the cursor back.
    body.push_str(r#"{"type":"user","uuid":"u4""#);
    std::fs::write(&file, &body).unwrap();

    let adapter = ClaudeCodeAdapter::with_root(tmp.join("projects"));
    assert_eq!(adapter.discover(), vec![file.clone()]);
    let (events, cursor) = adapter.poll(&file, 0).unwrap();
    assert_eq!(events.len(), 2, "only the two prose turns survive");
    assert_eq!(events[0].event_id, "u1");
    assert_eq!(events[0].role, Role::User);
    assert_eq!(events[1].event_id, "a1");
    assert_eq!(events[1].text, "Run it with --nocapture.");
    assert!(events[1].timestamp > events[0].timestamp);
    assert_eq!(events[0].raw_ref.path, file);
    let held_back = r#"{"type":"user","uuid":"u4""#.len() as u64;
    assert_eq!(cursor, std::fs::metadata(&file).unwrap().len() - held_back);

    // Completing the partial line and re-polling from the cursor yields
    // exactly the completed record.
    let mut full = std::fs::read_to_string(&file).unwrap();
    full.push_str(&format!(
        r#","sessionId":"s1","cwd":{:?},"timestamp":"2026-08-11T10:01:00Z","message":{{"role":"user","content":"and now?"}}}}"#,
        cwd.display().to_string()
    ));
    full.push('\n');
    std::fs::write(&file, &full).unwrap();
    let (delta, cursor2) = adapter.poll(&file, cursor).unwrap();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].event_id, "u4");
    assert_eq!(cursor2, std::fs::metadata(&file).unwrap().len());

    // Rotation: a shorter file resets to zero and reparses.
    std::fs::write(&file, lines[0].clone() + "\n").unwrap();
    let (rot, _) = adapter.poll(&file, cursor2).unwrap();
    assert_eq!(rot.len(), 1);
    assert_eq!(rot[0].event_id, "u1");

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn watch_roots_are_the_deduped_parents_of_discovered_transcripts() {
    use crate::harvest::{Harvester, claude_code::ClaudeCodeAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-watchroots-{}", id::new_id()));
    let projects = tmp.join("projects");
    // Two transcripts in one project directory, one in another — the watcher
    // should learn two directories, not three files.
    let a = projects.join("-Users-x-alpha");
    let b = projects.join("-Users-x-beta");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    for (dir, name) in [(&a, "s1.jsonl"), (&a, "s2.jsonl"), (&b, "s3.jsonl")] {
        std::fs::write(dir.join(name), "{}\n").unwrap();
    }

    let h = Harvester::new(vec![Box::new(ClaudeCodeAdapter::with_root(
        projects.clone(),
    ))]);
    let mut roots = h.watch_roots();
    roots.sort();
    assert_eq!(roots, vec![a.clone(), b.clone()], "parents, deduped");

    // Directories, not files: a session that does not exist yet is a file that
    // cannot be watched, so watching the parent is what catches its creation.
    assert!(roots.iter().all(|r| r.is_dir()));

    // Nothing discovered means nothing to arm — the caller skips the watcher
    // entirely and leans on the timed sweep.
    let empty = Harvester::new(vec![Box::new(ClaudeCodeAdapter::with_root(
        tmp.join("nowhere"),
    ))]);
    assert!(empty.watch_roots().is_empty());

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn harvest_sweep_end_to_end() {
    use crate::harvest::{Harvester, claude_code::ClaudeCodeAdapter};
    let mut env = crate::sync::EnvGuard::lock();
    let tmp = std::env::temp_dir().join(format!("engram-harvest-{}", id::new_id()));
    let _ = std::fs::remove_dir_all(&tmp);
    env.set("ENGRAM_HOME", tmp.join("enghome"));

    // A registered project the transcripts' cwd points into.
    let root = tmp.join("proj");
    let db = root.join(".engram/graph.tepin");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let entry = registry::register(&root, &db).unwrap();
    let canon_root = root.canonicalize().unwrap();

    let factory: EngineFactory = Box::new(|db: &std::path::Path| {
        let mut e = Engine::with_store(
            crate::store::open_store(db)?,
            Box::new(FakeEmbedder::default()),
        );
        e.set_history_path(crate::history::history_store_path(db));
        Ok(e)
    });
    let mut current = Engine::with_store(
        crate::store::open_store(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    current.set_history_path(crate::history::history_store_path(&db));
    enable_history(&current); // recording is opt-in; the config persists in the store
    let hub = Hub::new(
        std::sync::Arc::new(std::sync::Mutex::new(current)),
        Some(entry.clone()),
        Some(factory),
    );

    // One transcript: user turn then assistant turn, launched from a SUBDIR
    // of the repo (routing must prefix-match, not equality-match).
    let cc_root = tmp.join("claude/projects");
    let sess_dir = cc_root.join("-proj");
    std::fs::create_dir_all(&sess_dir).unwrap();
    let transcript = sess_dir.join("aaaa.jsonl");
    let sub = canon_root.join("frontend");
    std::fs::create_dir_all(&sub).unwrap();
    let mut body = [
        cc_record(
            "user",
            "u1",
            "sess-1",
            &sub,
            "2026-08-11T10:00:00Z",
            serde_json::json!("why does the daemon leak memory?"),
            &[],
        ),
        cc_record(
            "assistant",
            "a1",
            "sess-1",
            &sub,
            "2026-08-11T10:00:09Z",
            serde_json::json!([{ "type": "text", "text": "The batch width is the memory." }]),
            &[],
        ),
    ]
    .join("\n");
    body.push('\n');
    std::fs::write(&transcript, &body).unwrap();

    let mut harvester = Harvester::new(vec![Box::new(ClaudeCodeAdapter::with_root(
        cc_root.clone(),
    ))]);
    let stats = harvester.sweep(&hub);
    assert_eq!((stats.sessions, stats.messages, stats.unrouted), (1, 2, 0));

    let engine = hub.get(&entry.id).unwrap();
    let engine = engine.lock().unwrap();
    let (sessions, messages) = engine
        .with_history(|s| {
            let all = s.all_nodes().unwrap();
            (
                all.iter()
                    .filter(|n| n.node_type.as_str() == "Session")
                    .cloned()
                    .collect::<Vec<_>>(),
                all.iter()
                    .filter(|n| n.node_type.as_str() == "Message")
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(messages.len(), 2);
    // Titled by the first user message; props carry the running counters.
    assert_eq!(sessions[0].title, "why does the daemon leak memory?");
    let sp = sessions[0].props.as_ref().unwrap();
    assert_eq!(
        sp.get("messages").and_then(serde_json::Value::as_u64),
        Some(2)
    );
    assert_eq!(
        sp.get("harness").and_then(|v| v.as_str()),
        Some("claude-code")
    );
    // The chain: both messages `in` the session, u1 `next` a1.
    let in_edges = engine
        .with_history(|s| s.edges_in(&sessions[0].id).unwrap())
        .unwrap();
    assert_eq!(
        in_edges
            .iter()
            .filter(|e| e.edge_type.as_str() == "in")
            .count(),
        2
    );
    let user_msg = messages.iter().find(|m| m.source == Source::User).unwrap();
    let next: Vec<_> = engine
        .with_history(|s| s.edges_out(&user_msg.id).unwrap())
        .unwrap()
        .into_iter()
        .filter(|e| e.edge_type.as_str() == "next")
        .collect();
    assert_eq!(next.len(), 1);
    // History stays invisible to the curated surfaces.
    assert_eq!(engine.graph().unwrap().0.len(), 0);
    drop(engine);

    // Idempotence: a second sweep moves nothing (cursor holds).
    let stats2 = harvester.sweep(&hub);
    assert!(!stats2.wrote_anything(), "{stats2:?}");

    // Lost-cursor replay (fresh process, cursor file wiped): the seen-set
    // dedupe keeps the store byte-identical.
    std::fs::remove_file(tmp.join("enghome/history-cursors.json")).unwrap();
    let mut fresh = Harvester::new(vec![Box::new(ClaudeCodeAdapter::with_root(cc_root))]);
    let stats3 = fresh.sweep(&hub);
    assert!(!stats3.wrote_anything(), "{stats3:?}");
    let engine = hub.get(&entry.id).unwrap();
    let engine = engine.lock().unwrap();
    let count = engine
        .with_history(|s| s.all_nodes().unwrap().len())
        .unwrap();
    assert_eq!(count, 3, "replay wrote nothing new");

    env.remove("ENGRAM_HOME");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn history_search_and_session_listing_take_the_same_time_window() {
    use crate::history::{MESSAGE_TYPE, SESSION_TYPE};
    let dir = std::env::temp_dir().join(format!("engram-histwindow-{}", id::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.tepin");
    let mut e = Engine::new(
        TepinStore::open(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    e.set_history_path(crate::history::history_store_path(&db));
    enable_history(&e);

    let write = |type_name: &str, sid: &str, title: &str, ts: i64, extra: &[(&str, i64)]| {
        let mut props = serde_json::Map::new();
        props.insert("role".into(), "assistant".into());
        props.insert("turn".into(), 1.into());
        for (k, v) in extra {
            props.insert((*k).into(), (*v).into());
        }
        e.add_history_node(NewNode {
            node_type: NodeType::parse(type_name).unwrap(),
            title: title.into(),
            body: Some(format!("{title} — the daemon leaked memory")),
            created_at: Some(ts),
            durability: Durability::Stable,
            source: Source::Agent,
            session_id: Some(sid.into()),
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

    let june = day("2026-06-10");
    let august = day("2026-08-10");
    let old_msg = write(MESSAGE_TYPE, "sess-june", "the june leak", june, &[]);
    let new_msg = write(MESSAGE_TYPE, "sess-august", "the august leak", august, &[]);
    // Sessions carry a start and an end; the june one ran for an hour.
    write(
        SESSION_TYPE,
        "sess-june",
        "june session",
        june,
        &[("ended", june + 3600)],
    );
    write(
        SESSION_TYPE,
        "sess-august",
        "august session",
        august,
        &[("ended", august + 3600)],
    );

    let scoped = |after: Option<&str>, before: Option<&str>| {
        let filter = SearchFilter {
            window: crate::timespec::window(after, before, day("2026-09-01")).unwrap(),
            ..Default::default()
        };
        e.search_history_filtered("daemon leaked memory", 10, &filter)
            .unwrap()
            .into_iter()
            .map(|h| h.message_id)
            .collect::<Vec<_>>()
    };

    assert_eq!(scoped(None, None).len(), 2, "unscoped sees both");
    assert_eq!(scoped(Some("2026-08-01"), None), vec![new_msg.id.clone()]);
    assert_eq!(scoped(None, Some("2026-08-01")), vec![old_msg.id.clone()]);
    assert!(scoped(Some("2027-01-01"), None).is_empty());

    // The session lane list takes the same grammar, so "what was I doing then"
    // is answerable without a search hit to start from.
    let sessions = |after: Option<&str>, before: Option<&str>| {
        let w = crate::timespec::window(after, before, day("2026-09-01")).unwrap();
        e.list_history_sessions_in(w)
            .unwrap()
            .into_iter()
            .map(|s| s.session)
            .collect::<Vec<_>>()
    };
    assert_eq!(sessions(None, None).len(), 2);
    assert_eq!(sessions(Some("2026-08-01"), None), vec!["sess-august"]);
    assert_eq!(sessions(None, Some("2026-08-01")), vec!["sess-june"]);

    // Overlap, not containment: a window opening mid-session still finds the
    // session that was running through it.
    assert_eq!(
        sessions(Some("2026-06-10T00:30:00Z"), Some("2026-06-10T00:45:00Z")),
        vec!["sess-june"],
        "a session running through the window is inside it"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recent_ordering_folds_restatements_without_ever_cutting_one() {
    use crate::history::MESSAGE_TYPE;
    let dir = std::env::temp_dir().join(format!("engram-collapse-{}", id::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.tepin");
    let mut e = Engine::new(
        TepinStore::open(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    e.set_history_path(crate::history::history_store_path(&db));
    enable_history(&e);

    let msg = |text: &str, ts: i64| {
        let mut props = serde_json::Map::new();
        props.insert("role".into(), "assistant".into());
        props.insert("turn".into(), 1.into());
        e.add_history_node(NewNode {
            node_type: NodeType::parse(MESSAGE_TYPE).unwrap(),
            title: text.chars().take(40).collect(),
            body: Some(text.into()),
            created_at: Some(ts),
            durability: Durability::Stable,
            source: Source::Agent,
            session_id: Some(format!("sess-{ts}")),
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

    // The same fact restated across three sessions, plus one unrelated
    // message that must NOT be folded into it.
    let base = day("2026-08-01");
    msg("the daemon listens on port 8787", base);
    msg("the daemon listens on port 8787", base + 86_400);
    let newest = msg("the daemon listens on port 8787", base + 2 * 86_400);
    msg("the reranker is a cross encoder", base + 3 * 86_400);

    let recent = SearchFilter {
        order: crate::timespec::SearchOrder::Recent,
        ..Default::default()
    };
    let folded = e
        .search_history_filtered("the daemon listens on port 8787", 10, &recent)
        .unwrap();

    // The three restatements collapsed to one head — the newest — carrying the
    // older two as prior generations.
    let head = folded
        .iter()
        .find(|h| h.message_id == newest.id)
        .expect("the newest statement is the head");
    assert_eq!(head.prior.len(), 2, "both older forms folded under it");
    assert!(
        head.prior.iter().all(|p| p.timestamp < head.timestamp),
        "a prior generation is strictly older"
    );
    assert!(
        head.prior
            .windows(2)
            .all(|w| w[0].timestamp >= w[1].timestamp),
        "priors read newest-first"
    );

    // Nothing was cut: folding is presentation, so the same messages come back
    // either way — that invariant is what makes the threshold safe to ship
    // before it is measured.
    let flat = |hits: &[crate::history::HistoryHit]| {
        let mut ids: Vec<String> = hits
            .iter()
            .flat_map(|h| {
                std::iter::once(h.message_id.clone())
                    .chain(h.prior.iter().map(|p| p.message_id.clone()))
            })
            .collect();
        ids.sort();
        ids
    };
    let unfolded = e
        .search_history("the daemon listens on port 8787", 10)
        .unwrap();
    assert_eq!(flat(&folded), flat(&unfolded), "same set, different shape");
    assert!(folded.len() < unfolded.len(), "the shape really did change");

    // Relevance and chronological reads never fold.
    for order in [
        crate::timespec::SearchOrder::Relevance,
        crate::timespec::SearchOrder::Chronological,
    ] {
        let hits = e
            .search_history_filtered(
                "the daemon listens on port 8787",
                10,
                &SearchFilter {
                    order,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            hits.iter().all(|h| h.prior.is_empty()),
            "only the recency ordering folds"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn born_in_provenance_parks_and_resolves() {
    use crate::history::{MESSAGE_TYPE, VERB_BORN_IN};
    let dir = std::env::temp_dir().join(format!("engram-bornin-{}", id::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("graph.tepin");
    let mut e = Engine::new(
        TepinStore::open(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    e.set_history_path(crate::history::history_store_path(&db));
    enable_history(&e);

    let hist_msg = |sid: &str, role: &str, ts: i64| {
        let mut props = serde_json::Map::new();
        props.insert("role".into(), role.into());
        e.add_history_node(NewNode {
            node_type: NodeType::parse(MESSAGE_TYPE).unwrap(),
            title: format!("{role}@{ts}"),
            body: Some("text".into()),
            created_at: Some(ts),
            durability: Durability::Stable,
            source: Source::Agent,
            session_id: Some(sid.into()),
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
    let base = now() - 600;
    // Session A: assistant prose at base, base+10. Session B (concurrent,
    // dead after base+5): assistant prose at base+5.
    let a1 = hist_msg("sess-a", "assistant", base);
    let _b1 = hist_msg("sess-b", "assistant", base + 5);
    let note = e
        .add_node(new_node(NodeType::Decision, "we chose sibling stores", ""))
        .unwrap();
    // The note happened at base+8; nothing ingested past it yet — parked.
    e.park_provenance(&note.id, base + 8);
    assert_eq!(e.resolve_provenance().unwrap(), 0, "waits for catch-up");
    assert!(
        e.with_history(|s| s.edges_out(&note.id).unwrap())
            .unwrap()
            .is_empty()
    );
    // Session A continues past the note — catch-up reached. b1 (base+5) is
    // the closer preceding assistant, but session B died before the note;
    // the alive-session preference must pick a1 from session A instead.
    let _a2 = hist_msg("sess-a", "assistant", base + 10);
    assert_eq!(e.resolve_provenance().unwrap(), 1);
    let out = e.with_history(|s| s.edges_out(&note.id).unwrap()).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].edge_type.as_str(), VERB_BORN_IN);
    assert_eq!(
        out[0].to_id, a1.id,
        "alive session wins over closer dead one"
    );
    // The provenance handle the search hit renders.
    let born = e.born_in_of(&note.id).expect("born_in resolves");
    assert_eq!(born.message_id, a1.id);
    assert_eq!(born.session, "sess-a");
    // The lot drained — resolving again writes nothing.
    assert_eq!(e.resolve_provenance().unwrap(), 0);

    // A note with no preceding assistant message expires without an edge.
    let orphan = e
        .add_node(new_node(NodeType::Insight, "orphan", ""))
        .unwrap();
    e.park_provenance(&orphan.id, base - 100);
    assert_eq!(e.resolve_provenance().unwrap(), 0);
    assert!(
        e.with_history(|s| s.edges_out(&orphan.id).unwrap())
            .unwrap()
            .is_empty()
    );
    // Sanity: history layer off = park is a no-op.
    let mut cfg = e.graph_config();
    cfg.history.enabled = false;
    e.set_graph_config(&cfg).unwrap();
    e.park_provenance(&note.id, base);
    assert_eq!(e.resolve_provenance().unwrap(), 0);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn codex_adapter_keeps_prose_and_reads_meta_on_resume() {
    use crate::harvest::{HistoryAdapter, Role, codex::CodexAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-codex-{}", id::new_id()));
    let day = tmp.join("2026/08/11");
    std::fs::create_dir_all(&day).unwrap();
    let file = day.join("rollout-2026-08-11T10-00-00-abcd.jsonl");

    let lines = [
        r#"{"timestamp":"2026-08-11T10:00:00Z","type":"session_meta","payload":{"id":"cx-1","cwd":"/repo","originator":"codex"}}"#.to_string(),
        r#"{"timestamp":"2026-08-11T10:00:01Z","type":"event_msg","payload":{"type":"task_started"}}"#.to_string(),
        // Scaffolding wearing role user — dropped by its tag.
        r#"{"timestamp":"2026-08-11T10:00:02Z","type":"response_item","payload":{"type":"message","id":"m0","role":"user","content":[{"type":"input_text","text":"<environment_context>stuff</environment_context>"}]}}"#.to_string(),
        r#"{"timestamp":"2026-08-11T10:00:03Z","type":"response_item","payload":{"type":"message","id":"m1","role":"user","content":[{"type":"input_text","text":"why is the build red?"}]}}"#.to_string(),
        r#"{"timestamp":"2026-08-11T10:00:04Z","type":"response_item","payload":{"type":"reasoning","id":"r1","summary":[]}}"#.to_string(),
        r#"{"timestamp":"2026-08-11T10:00:05Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t1","name":"shell"}}"#.to_string(),
        r#"{"timestamp":"2026-08-11T10:00:06Z","type":"response_item","payload":{"type":"message","id":"m2","role":"assistant","content":[{"type":"output_text","text":"A missing semicolon."}]}}"#.to_string(),
        r#"{"timestamp":"2026-08-11T10:00:07Z","type":"response_item","payload":{"type":"message","id":"m3","role":"developer","content":[{"type":"input_text","text":"dev scaffolding"}]}}"#.to_string(),
    ];
    std::fs::write(&file, lines.join("\n") + "\n").unwrap();

    let adapter = CodexAdapter::with_root(tmp.clone());
    assert_eq!(adapter.discover(), vec![file.clone()]);
    let (events, cursor) = adapter.poll(&file, 0).unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].event_id, "m1");
    assert_eq!(events[0].role, Role::User);
    assert_eq!(events[0].session_id, "cx-1");
    assert_eq!(events[0].project_hint, std::path::PathBuf::from("/repo"));
    assert_eq!(events[1].text, "A missing semicolon.");
    assert_eq!(cursor, std::fs::metadata(&file).unwrap().len());

    // Mid-file resume: the delta has no session_meta line — the adapter
    // must recover session id + cwd from the file head.
    let mut all = std::fs::read_to_string(&file).unwrap();
    all.push_str(r#"{"timestamp":"2026-08-11T10:00:09Z","type":"response_item","payload":{"type":"message","id":"m4","role":"user","content":[{"type":"input_text","text":"ship it"}]}}"#);
    all.push('\n');
    std::fs::write(&file, &all).unwrap();
    let (delta, _) = adapter.poll(&file, cursor).unwrap();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].event_id, "m4");
    assert_eq!(delta[0].session_id, "cx-1");
    assert_eq!(delta[0].project_hint, std::path::PathBuf::from("/repo"));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn gemini_adapter_reads_chats_when_a_routing_fact_exists() {
    use crate::harvest::{HistoryAdapter, Role, gemini::GeminiAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-gemini-{}", id::new_id()));
    let chats = tmp.join("hash123/chats");
    std::fs::create_dir_all(&chats).unwrap();
    let routed = chats.join("session-1.json");
    std::fs::write(
        &routed,
        serde_json::json!({
            "sessionId": "g-1",
            "projectPath": "/repo",
            "startTime": "2026-08-11T09:00:00Z",
            "messages": [
                { "id": "u1", "role": "user", "content": "what changed in the daemon?" },
                { "id": "a1", "role": "model", "parts": [
                    { "functionCall": { "name": "run" } },
                    { "text": "The batch width was capped." }
                ]},
                { "id": "t1", "role": "tool", "content": "raw tool output" }
            ]
        })
        .to_string(),
    )
    .unwrap();
    // A chat with no routing fact is skipped, not hoarded.
    let unrouted = chats.join("session-2.json");
    std::fs::write(
        &unrouted,
        serde_json::json!({ "messages": [ { "role": "user", "content": "hi" } ] }).to_string(),
    )
    .unwrap();

    let adapter = GeminiAdapter::with_root(tmp.clone());
    assert_eq!(adapter.discover().len(), 2);
    let (events, cursor) = adapter.poll(&routed, 0).unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].role, Role::User);
    assert_eq!(events[1].text, "The batch width was capped.");
    assert_eq!(events[1].session_id, "g-1");
    assert_eq!(cursor, std::fs::metadata(&routed).unwrap().len());
    let (none, _) = adapter.poll(&unrouted, 0).unwrap();
    assert!(none.is_empty());

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn opencode_adapter_routes_through_the_session_index() {
    use crate::harvest::{HistoryAdapter, Role, opencode::OpencodeAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-oc-{}", id::new_id()));
    std::fs::create_dir_all(tmp.join("session/proj")).unwrap();
    std::fs::create_dir_all(tmp.join("message/ses_1")).unwrap();
    std::fs::write(
        tmp.join("session/proj/ses_1.json"),
        serde_json::json!({ "id": "ses_1", "directory": "/repo", "title": "t" }).to_string(),
    )
    .unwrap();
    std::fs::write(
        tmp.join("message/ses_1/msg_001.json"),
        serde_json::json!({
            "id": "msg_001", "sessionID": "ses_1", "role": "user",
            "time": { "created": 1786439732000i64 },
            "parts": [ { "type": "text", "text": "does the pane build?" },
                       { "type": "tool", "tool": "bash" } ]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        tmp.join("message/ses_1/msg_002.json"),
        serde_json::json!({
            "id": "msg_002", "sessionID": "ses_1", "role": "assistant",
            "time": { "created": 1786439735000i64 },
            "parts": [ { "type": "text", "text": "Yes, in 830ms." } ]
        })
        .to_string(),
    )
    .unwrap();

    let adapter = OpencodeAdapter::with_root(tmp.clone());
    let files = adapter.discover();
    assert_eq!(files.len(), 2);
    let (e1, _) = adapter.poll(&files[0], 0).unwrap();
    assert_eq!(e1.len(), 1);
    assert_eq!(e1[0].role, Role::User);
    assert_eq!(e1[0].project_hint, std::path::PathBuf::from("/repo"));
    assert_eq!(e1[0].timestamp, 1786439732);
    let (e2, _) = adapter.poll(&files[1], 0).unwrap();
    assert_eq!(e2[0].text, "Yes, in 830ms.");

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn kilo_adapter_parses_ui_messages_and_routes_via_editor_state() {
    use crate::harvest::{HistoryAdapter, Role, kilo::KiloAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-kilo-{}", id::new_id()));
    let storage = tmp.join("globalStorage");
    let task = storage.join("kilocode.kilo-code/tasks/task-1");
    std::fs::create_dir_all(&task).unwrap();
    let file = task.join("ui_messages.json");
    std::fs::write(
        &file,
        serde_json::json!([
            { "ts": 1786439732000i64, "type": "say", "say": "task",
              "text": "add a retry to the flaky test" },
            { "ts": 1786439733000i64, "type": "say", "say": "api_req_started",
              "text": "{\"request\":\"…\"}" },
            { "ts": 1786439734000i64, "type": "ask", "ask": "tool",
              "text": "{\"tool\":\"editedExistingFile\"}" },
            { "ts": 1786439740000i64, "type": "say", "say": "text",
              "text": "Retry added around the clock read." },
            { "ts": 1786439750000i64, "type": "say", "say": "user_feedback",
              "text": "also bump the timeout" },
            { "ts": 1786439760000i64, "type": "say", "say": "completion_result",
              "text": "Done — retry and timeout both in." }
        ])
        .to_string(),
    )
    .unwrap();
    // The editor's global state names the task's workspace.
    let conn = rusqlite::Connection::open(storage.join("state.vscdb")).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable VALUES ('kilocode.kilo-code', ?1)",
        [serde_json::json!({
            "taskHistory": [ { "id": "task-1", "workspace": "/repo" } ]
        })
        .to_string()],
    )
    .unwrap();
    drop(conn);

    let adapter = KiloAdapter::with_roots(vec![storage.clone()]);
    assert_eq!(adapter.discover(), vec![file.clone()]);
    let (events, cursor) = adapter.poll(&file, 0).unwrap();
    // Tool traffic (api_req_started, asks) drops; four prose turns survive.
    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(events[0].role, Role::User);
    assert_eq!(events[0].text, "add a retry to the flaky test");
    assert_eq!(events[0].timestamp, 1786439732);
    assert_eq!(events[0].project_hint, std::path::PathBuf::from("/repo"));
    assert_eq!(events[1].role, Role::Assistant);
    assert_eq!(events[2].role, Role::User);
    assert_eq!(events[3].text, "Done — retry and timeout both in.");
    assert!(events.iter().all(|e| e.session_id == "task-1"));
    // Whole-file reparse keeps identities stable (the seen-set's contract).
    let (again, _) = adapter.poll(&file, cursor).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| e.event_id.clone())
            .collect::<Vec<_>>(),
        again.iter().map(|e| e.event_id.clone()).collect::<Vec<_>>()
    );

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn antigravity_adapter_parses_transcript_steps_and_holds_partial_tail() {
    use crate::harvest::{HistoryAdapter, Role, antigravity::AntigravityAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-agy-{}", id::new_id()));
    let home = tmp.join("antigravity-cli");
    let logs = home.join("brain/conv-1/.system_generated/logs");
    std::fs::create_dir_all(&logs).unwrap();
    // The conversation → workspace map the transcript itself never carries.
    std::fs::write(
        home.join("history.jsonl"),
        concat!(
            "{\"display\":\"/mcp\",\"timestamp\":1,\"workspace\":\"/elsewhere\"}\n",
            "{\"display\":\"why is the daemon leaking?\",\"timestamp\":2,",
            "\"workspace\":\"/repo\",\"conversationId\":\"conv-1\"}\n"
        ),
    )
    .unwrap();
    let file = logs.join("transcript.jsonl");
    let user_step = serde_json::json!({
        "step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
        "status": "DONE", "created_at": "2026-08-11T10:00:00Z",
        "content": "<USER_REQUEST>\nwhy is the daemon leaking?\n</USER_REQUEST>\n\
                    <ADDITIONAL_METADATA>\nThe current local time is X.\n</ADDITIONAL_METADATA>"
    });
    let body = format!(
        "{user_step}\n\
         {}\n\
         {}\n\
         {}\n\
         {}\n\
         {{\"step_index\":5,\"type\":\"PLANNER_RESPO",
        serde_json::json!({ "step_index": 1, "source": "SYSTEM",
            "type": "CONVERSATION_HISTORY", "created_at": "2026-08-11T10:00:00Z" }),
        serde_json::json!({ "step_index": 2, "source": "MODEL",
            "type": "PLANNER_RESPONSE", "created_at": "2026-08-11T10:00:01Z",
            "tool_calls": [{ "name": "call_mcp_tool" }] }),
        serde_json::json!({ "step_index": 3, "source": "MODEL", "type": "MCP_TOOL",
            "created_at": "2026-08-11T10:00:02Z", "content": "Created At: …" }),
        serde_json::json!({ "step_index": 4, "source": "MODEL",
            "type": "PLANNER_RESPONSE", "created_at": "2026-08-11T10:00:09Z",
            "content": "The batch width is the memory." }),
    );
    std::fs::write(&file, &body).unwrap();

    let adapter = AntigravityAdapter::with_root(home.clone());
    assert_eq!(adapter.discover(), vec![file.clone()]);
    let (events, cursor) = adapter.poll(&file, 0).unwrap();
    // Steps 1–3 are machine traffic; step 5 is a partial tail — held back.
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].role, Role::User);
    assert_eq!(events[0].text, "why is the daemon leaking?");
    assert_eq!(events[0].timestamp, 1786442400);
    assert_eq!(events[0].project_hint, std::path::PathBuf::from("/repo"));
    assert_eq!(events[1].role, Role::Assistant);
    assert_eq!(events[1].text, "The batch width is the memory.");
    assert!(events.iter().all(|e| e.session_id == "conv-1"));
    assert!(cursor < body.len() as u64, "partial tail holds the cursor");

    // The writer finishes the line: only the delta parses.
    let mut all = body.clone();
    all.truncate(cursor as usize);
    all.push_str(
        &serde_json::json!({ "step_index": 5, "source": "MODEL",
            "type": "PLANNER_RESPONSE", "created_at": "2026-08-11T10:00:12Z",
            "content": "Confirmed: capping the batch fixes it." })
        .to_string(),
    );
    all.push('\n');
    std::fs::write(&file, &all).unwrap();
    let (delta, _) = adapter.poll(&file, cursor).unwrap();
    assert_eq!(delta.len(), 1, "{delta:?}");
    assert_eq!(delta[0].text, "Confirmed: capping the batch fixes it.");
    assert_eq!(delta[0].event_id, "step-5");

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn bob_adapter_reads_the_shared_sqlite_and_cursors_by_timestamp() {
    use crate::harvest::{HistoryAdapter, Role, bob::BobAdapter};
    let tmp = std::env::temp_dir().join(format!("engram-bob-{}", id::new_id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let db = tmp.join("bob.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE tasks (id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
             directory TEXT NOT NULL DEFAULT '', task_type TEXT NOT NULL DEFAULT 'normal');
         CREATE TABLE messages (id TEXT PRIMARY KEY, task_id TEXT NOT NULL,
             role TEXT NOT NULL, data TEXT NOT NULL, created_at INTEGER NOT NULL);
         INSERT INTO tasks VALUES
             ('t1', 'file:/repo', '', 'normal'),
             ('t2', 'file:/repo', '', 'subagent'),
             ('t3', '', '', 'normal');",
    )
    .unwrap();
    let insert = |id: &str, task: &str, role: &str, data: serde_json::Value, ms: i64| {
        conn.execute(
            "INSERT INTO messages VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, task, role, data.to_string(), ms],
        )
        .unwrap();
    };
    // Bob batches a flush under one created_at; _meta carries the moment.
    insert(
        "m1",
        "t1",
        "system",
        serde_json::json!({"role": "system", "content": "scaffold"}),
        1_786_400_000_000,
    );
    insert(
        "m2",
        "t1",
        "user",
        serde_json::json!({"role": "user", "content": "why does the daemon leak?", "envContext": "<environment_details>…</environment_details>"}),
        1_786_400_000_000,
    );
    insert(
        "m3",
        "t1",
        "assistant",
        serde_json::json!({"role": "assistant", "content": "", "toolCalls": [{"name": "grep"}]}),
        1_786_400_000_000,
    );
    insert(
        "m4",
        "t1",
        "tool",
        serde_json::json!({"role": "tool", "content": "grep output"}),
        1_786_400_000_000,
    );
    insert(
        "m5",
        "t1",
        "assistant",
        serde_json::json!({"role": "assistant", "content": "The batch width is the memory.", "_meta": {"timestamp": 1_786_400_009_000i64}}),
        1_786_400_010_000,
    );
    insert(
        "m6",
        "t2",
        "user",
        serde_json::json!({"role": "user", "content": "sidechain task"}),
        1_786_400_011_000,
    );
    insert(
        "m7",
        "t3",
        "user",
        serde_json::json!({"role": "user", "content": "uri-less playground"}),
        1_786_400_012_000,
    );
    drop(conn);

    let adapter = BobAdapter::with_db(db.clone());
    assert_eq!(adapter.discover(), vec![db.clone()]);
    let (events, cursor) = adapter.poll(&db, 0).unwrap();
    // system/tool never leave the query; the empty tool-call assistant, the
    // subagent task and the URI-less task all drop.
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].role, Role::User);
    assert_eq!(events[0].text, "why does the daemon leak?");
    assert_eq!(events[0].timestamp, 1_786_400_000);
    assert_eq!(events[0].project_hint, std::path::PathBuf::from("/repo"));
    assert_eq!(events[1].role, Role::Assistant);
    assert_eq!(
        events[1].timestamp, 1_786_400_009,
        "_meta beats the flush time"
    );
    assert!(events.iter().all(|e| e.session_id == "t1"));
    // The cursor is the max row timestamp seen — including filtered rows.
    assert_eq!(cursor, 1_786_400_012_000);
    // Delta poll from the frontier re-reads only the boundary millisecond;
    // the harvester's seen-set owns dedupe, identities must hold stable.
    let (again, c2) = adapter.poll(&db, cursor).unwrap();
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(c2, cursor);
    let (replay, _) = adapter.poll(&db, 0).unwrap();
    assert_eq!(
        replay
            .iter()
            .map(|e| e.event_id.clone())
            .collect::<Vec<_>>(),
        events
            .iter()
            .map(|e| e.event_id.clone())
            .collect::<Vec<_>>()
    );

    std::fs::remove_dir_all(&tmp).ok();
}
