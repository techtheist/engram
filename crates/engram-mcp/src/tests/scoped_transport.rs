use crate::*;
use engram_core::{FakeEmbedder, SqliteStore, registry};
use rmcp::model::CallToolRequestParams;

/// v0.6.2: a session on /projects/{id}/mcp treats that project as current
/// — the repo-bound AI side of "one core, one pane".
#[tokio::test]
async fn scoped_mcp_endpoint_binds_sessions_to_their_project() {
    let tmp = std::env::temp_dir().join(format!("engram-mcp-scope-{}", std::process::id()));
    let beta_root = tmp.join("beta");
    std::fs::create_dir_all(beta_root.join(".engram")).unwrap();
    let mut env = engram_core::sync::EnvGuard::lock();
    env.set("ENGRAM_HOME", tmp.join("home"));
    let beta_db = beta_root.join(".engram/graph.db");
    registry::register(&beta_root, &beta_db).unwrap();
    {
        let beta = Engine::new(
            SqliteStore::open(&beta_db).unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        beta.add_node(engram_core::NewNode {
            version: None,
            created_at: None,
            node_type: engram_core::NodeType::Decision,
            title: "beta owns this decision".into(),
            body: None,
            durability: engram_core::Durability::Stable,
            source: engram_core::Source::Agent,
            session_id: None,
            status: None,
            code_refs: vec![],
            tags: vec![],
            props: None,
            fields: None,
        })
        .unwrap();
    }

    let alpha = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let factory: engram_core::EngineFactory = Box::new(|db| {
        Ok(Engine::new(
            SqliteStore::open(db)?,
            Box::new(FakeEmbedder::default()),
        ))
    });
    let hub = Arc::new(Hub::new(Arc::new(Mutex::new(alpha)), None, Some(factory)));

    let app = axum::Router::new()
        .route_service("/mcp", streamable_http_service(hub.clone()))
        .route_service(
            "/projects/beta/mcp",
            streamable_http_service_for(hub.clone(), "beta".into()),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let scoped = ()
        .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
            format!("http://{addr}/projects/beta/mcp"),
        ))
        .await
        .unwrap();
    let hits = scoped
        .peer()
        .call_tool(
            CallToolRequestParams::new("search").with_arguments(
                serde_json::json!({ "query": "beta owns decision" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let text = format!("{:?}", hits.content);
    assert!(
        text.contains("beta owns this decision"),
        "scoped session searches ITS project by default: {text}"
    );

    // 0.8.8 regression guard: the SESSION's binding — not the hub's own
    // current project — is what an unscoped `brief` renders and what
    // `list_projects` calls current. Both used to read `hub.current`,
    // which was invisibly right while the daemon launched inside the repo
    // it served and became always-wrong once the machine core became a
    // dedicated home-rooted process: every bound session was handed the
    // home graph's cold-start brief and told it was sitting in `home`.
    let brief = scoped
        .peer()
        .call_tool(CallToolRequestParams::new("brief"))
        .await
        .unwrap();
    let text = format!("{:?}", brief.content);
    assert!(
        text.contains("beta owns this decision"),
        "an unscoped brief on a bound session briefs THAT project: {text}"
    );

    // …and a `project` argument accepts the project's DIRECTORY, so a
    // caller holding a folder (a hook, a bridge's cwd) never has to map
    // it to a name first.
    let by_dir = scoped
        .peer()
        .call_tool(
            CallToolRequestParams::new("brief").with_arguments(
                serde_json::json!({ "project": beta_root.display().to_string() })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let by_dir = format!("{:?}", by_dir.content);
    assert!(
        by_dir.contains("beta owns this decision"),
        "a directory names its project: {by_dir}"
    );

    let roster = scoped
        .peer()
        .call_tool(CallToolRequestParams::new("list_projects"))
        .await
        .unwrap();
    let text = format!("{:?}", roster.content);
    // One pretty-printed JSON object per project, so splitting on the
    // brace puts each row's name beside its own `current` flag.
    let row = |name: &str| {
        text.split('{')
            .find(|r| r.contains(&format!("\\\"name\\\": \\\"{name}\\\"")))
            .unwrap_or_else(|| panic!("no {name} row in {text}"))
            .to_string()
    };
    assert!(
        row("beta").contains("\\\"current\\\": true"),
        "the bound project is the current one: {text}"
    );
    assert!(
        row("home").contains("\\\"current\\\": false"),
        "the core's own launch graph is not this session's project: {text}"
    );

    let unscoped = ()
        .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
            format!("http://{addr}/mcp"),
        ))
        .await
        .unwrap();
    let hits = unscoped
        .peer()
        .call_tool(
            CallToolRequestParams::new("search").with_arguments(
                serde_json::json!({ "query": "beta owns decision" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let text = format!("{:?}", hits.content);
    assert!(
        !text.contains("beta owns this decision"),
        "the hub's current project stays its own graph: {text}"
    );

    // A scoped `brief` rebinds the session ITSELF (0.8.9's set_project,
    // folded into brief in 0.8.11) — the recovery for clients that
    // advertise roots but never answer them (issue #4). An unknown
    // selector is refused with the known roster and leaves the binding
    // untouched…
    let err = unscoped
        .peer()
        .call_tool(
            CallToolRequestParams::new("brief").with_arguments(
                serde_json::json!({ "project": "definitely-not-a-project" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap_err();
    let err = err.to_string();
    assert!(err.contains("beta"), "refusal lists the roster: {err}");

    // …a directory selector rebinds, and the response carries the brief,
    // so one call yields binding + memory context.
    let bound = unscoped
        .peer()
        .call_tool(
            CallToolRequestParams::new("brief").with_arguments(
                serde_json::json!({ "project": beta_root.display().to_string() })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let text = format!("{:?}", bound.content);
    // Leaving a real project for a sibling WARNS: the next
    // write goes to beta, and the line names the way back.
    assert!(
        text.contains("this session's writes now go to 'beta'"),
        "a scoped brief that moves the session warns: {text}"
    );
    assert!(
        text.contains("no longer to") && text.contains("go back now with brief(project:"),
        "the warning names the graph left and the call that returns: {text}"
    );
    assert!(
        text.contains("beta owns this decision"),
        "a scoped brief returns the target's brief: {text}"
    );

    let hits = unscoped
        .peer()
        .call_tool(
            CallToolRequestParams::new("search").with_arguments(
                serde_json::json!({ "query": "beta owns decision" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let text = format!("{:?}", hits.content);
    assert!(
        text.contains("beta owns this decision"),
        "after the rebind the session's default project IS beta: {text}"
    );

    // Re-briefing the project the session is already on is a plain
    // confirmation, never a warning.
    let again = unscoped
        .peer()
        .call_tool(
            CallToolRequestParams::new("brief").with_arguments(
                serde_json::json!({ "project": "beta" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let text = format!("{:?}", again.content);
    assert!(
        text.contains("Session bound to 'beta'") && !text.contains("Warning"),
        "the same project re-briefed stays quiet: {text}"
    );

    // The census follows the rebind: no live session still claims the
    // launch binding; the moved session reports beta.
    let sessions = hub.sessions();
    assert!(
        sessions.iter().any(|s| s.name == "beta"),
        "census carries the rebind: {sessions:?}"
    );
    assert!(
        !sessions.iter().any(|s| s.project == "current"),
        "no session still claims the launch binding: {sessions:?}"
    );

    env.remove("ENGRAM_HOME");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The fallback hint: a session that landed on the home graph (the
/// ladder's last rung) is told, atop its brief, how to recover — and a
/// properly bound session never sees the line.
#[tokio::test]
async fn home_bound_session_brief_carries_the_rebind_hint() {
    let factory: engram_core::EngineFactory = Box::new(|db| {
        Ok(Engine::new(
            SqliteStore::open(db)?,
            Box::new(FakeEmbedder::default()),
        ))
    });
    let home = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hub = Arc::new(Hub::new_home(Arc::new(Mutex::new(home)), Some(factory)));
    let s = Engram::with_hub(hub);
    let brief = s
        .brief(Parameters(BriefArgs {
            max_chars: None,
            project: None,
        }))
        .await
        .unwrap();
    let text = format!("{:?}", brief.content);
    assert!(
        text.contains("rebinds this session"),
        "home-bound sessions are taught the recovery: {text}"
    );

    // Single-project processes have no other projects to rebind to.
    let single = Engram::new(Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    ));
    let brief = single
        .brief(Parameters(BriefArgs {
            max_chars: None,
            project: None,
        }))
        .await
        .unwrap();
    let text = format!("{:?}", brief.content);
    assert!(
        !text.contains("rebinds this session"),
        "a single-project session never sees the hint: {text}"
    );
}

/// Issue #11: a session that fell back to the home graph (the bridge
/// announced `bound_by=home` — Windsurf answered no roots, the launch
/// dir can't host a project) refuses writes with a teaching error,
/// answers reads, and writes again once the agent binds it with a
/// scoped `brief` — explicit `project` targets pass throughout.
#[tokio::test]
async fn home_fallback_session_refuses_writes_until_briefed() {
    let factory: engram_core::EngineFactory = Box::new(|db| {
        Ok(Engine::new(
            SqliteStore::open(db)?,
            Box::new(FakeEmbedder::default()),
        ))
    });
    let home = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hub = Arc::new(Hub::new_home(Arc::new(Mutex::new(home)), Some(factory)));
    let s = Engram::with_hub(hub.clone());
    s.record_client(&Implementation::new("Windsurf", "1.0.0").with_description("bound_by=home"));

    // Census legibility rides the same handshake.
    let row = hub
        .sessions()
        .into_iter()
        .find(|r| r.session_id == *s.session_id)
        .expect("the session is in the census");
    assert_eq!(row.client.as_deref(), Some("Windsurf"));
    assert_eq!(row.bound_by.as_deref(), Some("home"));

    let note = |title: &str, project: Option<&str>| AddNoteArgs {
        node_type: "Decision".into(),
        title: title.into(),
        body: None,
        tags: vec![],
        code_refs: vec![],
        durability: None,
        version: None,
        created_at: None,
        session_id: None,
        fields: None,
        project: project.map(str::to_string),
    };
    let err = s
        .add_note(Parameters(note("lands nowhere", None)))
        .await
        .expect_err("an unbound session refuses the write");
    assert!(
        err.message.contains("write refused")
            && err.message.contains("Windsurf")
            && err.message.contains("`brief`"),
        "the refusal teaches the fix: {}",
        err.message
    );
    for name in ["link", "update_node", "set_version"] {
        let refused = match name {
            "link" => s
                .link(Parameters(LinkArgs {
                    from: "a".into(),
                    to: "b".into(),
                    edge_type: "because".into(),
                    note: None,
                    confidence: None,
                    project: None,
                }))
                .await
                .is_err(),
            "update_node" => s
                .update_node(Parameters(UpdateArgs {
                    id: "a".into(),
                    node_type: None,
                    title: Some("x".into()),
                    body: None,
                    durability: None,
                    status: None,
                    code_refs: None,
                    tags: None,
                    version: None,
                    fields: None,
                    project: None,
                    reviewed: None,
                }))
                .await
                .is_err(),
            _ => s
                .set_version(Parameters(SetVersionArgs {
                    version: Some("1.0".into()),
                    project: None,
                }))
                .await
                .is_err(),
        };
        assert!(refused, "{name} is refused while unbound");
    }
    // Reads answer, and the brief's hint says writes are refused.
    let brief = s
        .brief(Parameters(BriefArgs {
            max_chars: None,
            project: None,
        }))
        .await
        .unwrap();
    let text = format!("{:?}", brief.content);
    assert!(
        text.contains("WRITES ARE REFUSED") && text.contains("Windsurf"),
        "the brief hint names the client and the refusal: {text}"
    );
    assert!(
        text.contains("the home graph (user-level, no project)"),
        "the brief's first line names the graph: {text}"
    );
    // An explicit target is deliberate — it passes even now.
    let explicit = s
        .add_note(Parameters(note("deliberately in home", Some("home"))))
        .await
        .expect("explicit project passes");
    assert!(
        format!("{:?}", explicit.content).contains("\\\"project\\\": \\\"home\\\""),
        "the verdict names the graph: {:?}",
        explicit.content
    );
    // The agent binds the session: writes flow, the hint is gone.
    s.brief(Parameters(BriefArgs {
        max_chars: None,
        project: Some("home".into()),
    }))
    .await
    .expect("brief(project) rebinds");
    let ok = s
        .add_note(Parameters(note("after binding", None)))
        .await
        .expect("bound sessions write");
    assert!(
        format!("{:?}", ok.content).contains("\\\"project\\\": \\\"home\\\""),
        "every write names its graph: {:?}",
        ok.content
    );
    let brief = s
        .brief(Parameters(BriefArgs {
            max_chars: None,
            project: None,
        }))
        .await
        .unwrap();
    assert!(
        !format!("{:?}", brief.content).contains("by fallback"),
        "a session the agent bound itself sees no fallback hint"
    );
    let row = hub
        .sessions()
        .into_iter()
        .find(|r| r.session_id == *s.session_id)
        .unwrap();
    assert_eq!(row.bound_by.as_deref(), Some("brief"));
}

/// The default-agent-project rung is a user's explicit setting, so it
/// writes — but the first write of the session says the workspace was
/// never confirmed, once.
#[tokio::test]
async fn default_project_session_notes_its_first_write_only() {
    let s = crate::tests::tools::server();
    s.record_client(
        &Implementation::new("Windsurf", "1.0.0").with_description("bound_by=default-project"),
    );
    let note = |title: &str| AddNoteArgs {
        node_type: "Decision".into(),
        title: title.into(),
        body: None,
        tags: vec![],
        code_refs: vec![],
        durability: None,
        version: None,
        created_at: None,
        session_id: None,
        fields: None,
        project: None,
    };
    let first = format!(
        "{:?}",
        s.add_note(Parameters(note("first write")))
            .await
            .unwrap()
            .content
    );
    assert!(
        first.contains("binding_note") && first.contains("default-agent-project"),
        "the first write carries the note: {first}"
    );
    let second = format!(
        "{:?}",
        s.add_note(Parameters(note("second write")))
            .await
            .unwrap()
            .content
    );
    assert!(
        !second.contains("binding_note") && second.contains("\\\"project\\\""),
        "the second write only names the graph: {second}"
    );
}

#[test]
fn bound_by_round_trips_through_the_client_description() {
    for rung in [
        BoundBy::Db,
        BoundBy::Roots,
        BoundBy::Cwd,
        BoundBy::DefaultProject,
        BoundBy::Home,
        BoundBy::Unwired,
        BoundBy::Brief,
    ] {
        assert_eq!(
            BoundBy::from_description(Some(&rung.announce())),
            Some(rung)
        );
    }
    assert_eq!(BoundBy::from_description(None), None);
    assert_eq!(BoundBy::from_description(Some("a plain client")), None);
    assert_eq!(
        BoundBy::from_description(Some("x=1; bound_by=roots; y=2")),
        Some(BoundBy::Roots)
    );
    assert_eq!(
        BoundBy::from_description(Some("bound_by=something-newer")),
        Some(BoundBy::Route),
        "an unknown rung from a newer bridge reads as the route alone"
    );
    assert!(BoundBy::Home.refuses_writes());
    assert!(BoundBy::Unwired.refuses_writes());
    assert!(!BoundBy::DefaultProject.refuses_writes());
    assert!(!BoundBy::Roots.refuses_writes());
}
