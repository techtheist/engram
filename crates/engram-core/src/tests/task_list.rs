//! Tasks, the working list beside the graph.

use super::*;

pub(super) fn task(title: &str) -> crate::tasks::NewTask {
    crate::tasks::NewTask {
        title: title.into(),
        ..Default::default()
    }
}

pub(super) fn one_task(e: &Engine, t: crate::tasks::NewTask) -> String {
    let w = e.add_tasks(vec![t]).unwrap().pop().unwrap();
    assert!(w.created, "{w:?}");
    w.id.unwrap()
}

#[test]
fn tasks_stay_out_of_the_graph_and_lead_the_brief() {
    let e = engine();
    e.set_current_version(Some("0.10.0")).unwrap();
    let id = one_task(
        &e,
        crate::tasks::NewTask {
            body: Some("a long plan body that the brief must not print".into()),
            ..task("Build the tasks screen")
        },
    );
    one_task(
        &e,
        crate::tasks::NewTask {
            status: Some("backlog".into()),
            ..task("Someday: tasks in the JetBrains tool window")
        },
    );
    // Not knowledge: no node, no search hit.
    assert_eq!(e.store().stats().unwrap().nodes, 0);
    let brief = e.brief(16_000).unwrap();
    assert!(brief.contains("## Tasks"), "{brief}");
    assert!(
        brief.contains(&format!("- Build the tasks screen [Task {id}]")),
        "{brief}"
    );
    assert!(!brief.contains("a long plan body"), "titles only: {brief}");
    assert!(
        !brief.contains("JetBrains tool window"),
        "backlog is counted, not listed"
    );
    assert!(brief.contains("Parked: 0 delayed, 1 backlog"), "{brief}");

    // Turning the section off removes it.
    let mut cfg = e.graph_config();
    cfg.brief.tasks.show = false;
    e.set_graph_config(&cfg).unwrap();
    assert!(!e.brief(16_000).unwrap().contains("## Tasks"));
}

#[test]
fn an_empty_task_list_leaves_the_brief_unchanged() {
    let e = engine();
    e.add_node(new_node(NodeType::Decision, "Use sqlite", "wal"))
        .unwrap();
    assert!(!e.brief(16_000).unwrap().contains("## Tasks"));
}

#[test]
fn groups_are_one_level_and_follow_their_children() {
    let e = engine();
    let out = e
        .add_tasks(vec![
            task("Tasks feature"),
            crate::tasks::NewTask {
                parent: Some("$1".into()),
                ..task("Core entity and store drivers")
            },
            crate::tasks::NewTask {
                parent: Some("$1".into()),
                ..task("Pane board screen")
            },
        ])
        .unwrap();
    let ids: Vec<String> = out.iter().map(|w| w.id.clone().unwrap()).collect();
    let (group, a, b) = (&ids[0], &ids[1], &ids[2]);

    // A group inside a group is refused, as is a group moving under one.
    let nested = e
        .add_tasks(vec![crate::tasks::NewTask {
            parent: Some(a.clone()),
            ..task("Too deep")
        }])
        .unwrap();
    assert!(
        nested[0].error.as_deref().unwrap().contains("one level"),
        "{nested:?}"
    );
    let other = one_task(&e, task("Unrelated release chores"));
    let err = e
        .update_task(
            group,
            crate::tasks::TaskPatch {
                parent: Some(Some(other.clone())),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("is a group"), "{err}");

    let brief = e.brief(16_000).unwrap();
    assert!(
        brief.contains(&format!(
            "- Tasks feature [Task {group}] — 0/2 done, next: Core entity and store drivers [Task {a}]"
        )),
        "{brief}"
    );

    let done = |id: &str| {
        e.update_task(
            id,
            crate::tasks::TaskPatch {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .unwrap()
    };
    done(a);
    assert_eq!(
        e.get_task(group).unwrap().unwrap().status,
        TaskStatus::Active
    );
    done(b);
    let g = e.get_task(group).unwrap().unwrap();
    assert_eq!(
        g.status,
        TaskStatus::Done,
        "every child closed closes the group"
    );
    assert!(g.auto.unwrap().contains("every task in the group closed"));

    // Reopening a child reopens the group.
    e.update_task(
        b,
        crate::tasks::TaskPatch {
            status: Some(TaskStatus::Active),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        e.get_task(group).unwrap().unwrap().status,
        TaskStatus::Active
    );

    // Dropping the group by hand drops its open children with it.
    e.update_task(
        group,
        crate::tasks::TaskPatch {
            status: Some(TaskStatus::Dropped),
            ..Default::default()
        },
    )
    .unwrap();
    let child = e.get_task(b).unwrap().unwrap();
    assert_eq!(child.status, TaskStatus::Dropped);
    assert_eq!(
        e.get_task(a).unwrap().unwrap().status,
        TaskStatus::Done,
        "closed stay closed"
    );
    assert_eq!(
        e.get_task(group).unwrap().unwrap().status,
        TaskStatus::Dropped
    );
}

#[test]
fn version_rules_fire_on_set_version() {
    let e = engine();
    e.set_current_version(Some("0.9.12")).unwrap();
    // A release-shaped title closes itself once the project moves past it.
    let w = e
        .add_tasks(vec![task("Publish 0.9.12")])
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(w.auto_close.as_deref(), Some(">0.9.12"));
    let publish = w.id.unwrap();
    let explicit = one_task(
        &e,
        crate::tasks::NewTask {
            close: Some("0.10.1".into()),
            ..task("Ship the board")
        },
    );
    let parked = one_task(
        &e,
        crate::tasks::NewTask {
            status: Some("delayed".into()),
            wake: Some("0.10.0".into()),
            ..task("Re-run the ladder on the new embedder")
        },
    );
    let carried = one_task(&e, task("Long-running research"));

    e.set_current_version(Some("0.10.0")).unwrap();
    let p = e.get_task(&publish).unwrap().unwrap();
    assert_eq!(p.status, TaskStatus::Done);
    assert_eq!(p.closed_version.as_deref(), Some("0.10.0"));
    assert!(
        p.auto.as_deref().unwrap().contains("moved past 0.9.12"),
        "{p:?}"
    );
    assert!(p.close.is_none(), "a fired rule clears");
    assert_eq!(
        e.get_task(&explicit).unwrap().unwrap().status,
        TaskStatus::Active
    );
    let w = e.get_task(&parked).unwrap().unwrap();
    assert_eq!(w.status, TaskStatus::Active);
    assert!(w.auto.unwrap().starts_with("woke:"));

    let brief = e.brief(16_000).unwrap();
    assert!(
        brief.contains(&format!(
            "Long-running research [Task {carried} · ↻ from 0.9.12]"
        )),
        "{brief}"
    );
    assert!(
        brief.contains("Recently done: Publish 0.9.12 (done:"),
        "{brief}"
    );

    e.set_current_version(Some("0.10.1")).unwrap();
    assert_eq!(
        e.get_task(&explicit).unwrap().unwrap().status,
        TaskStatus::Done
    );
}

#[test]
fn date_and_node_rules_settle_at_read_time() {
    let e = engine();
    let problem = e
        .add_node(NewNode {
            status: Some(NodeStatus::Open),
            ..new_node(NodeType::Problem, "Flaky e2e on CI", "")
        })
        .unwrap();
    let by_node = one_task(
        &e,
        crate::tasks::NewTask {
            close: Some(format!("node:{}", problem.id)),
            refs: vec![problem.id.clone()],
            ..task("Chase the flaky e2e")
        },
    );
    let by_date = one_task(
        &e,
        crate::tasks::NewTask {
            status: Some("delayed".into()),
            wake: Some("2020-01-01".into()),
            ..task("Revisit the embedder choice")
        },
    );
    // The past date is due on the first read.
    let listed = e.list_tasks(&TaskFilter::default()).unwrap();
    let woke = listed.iter().find(|v| v.task.id == by_date).unwrap();
    assert_eq!(woke.task.status, TaskStatus::Active);
    assert_eq!(
        e.get_task(&by_node).unwrap().unwrap().status,
        TaskStatus::Active
    );

    e.update_node(
        &problem.id,
        NodePatch {
            status: Some(NodeStatus::Resolved),
            ..Default::default()
        },
    )
    .unwrap();
    let refd = e
        .list_tasks(&TaskFilter {
            statuses: vec![TaskStatus::Done],
            ref_id: Some(problem.id.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(refd.len(), 1, "{refd:?}");
    assert!(refd[0].task.auto.as_deref().unwrap().contains("closed"));

    // Refs must name real nodes; rules must parse.
    let bad = e
        .add_tasks(vec![
            crate::tasks::NewTask {
                refs: vec!["nope".into()],
                ..task("Bad refs")
            },
            crate::tasks::NewTask {
                close: Some("whenever".into()),
                ..task("Bad rule")
            },
        ])
        .unwrap();
    assert!(bad[0].error.as_deref().unwrap().contains("nope"));
    assert!(
        bad[1]
            .error
            .as_deref()
            .unwrap()
            .contains("can't read the rule")
    );
}

#[test]
fn a_reworded_open_task_matches_instead_of_duplicating() {
    let e = engine();
    let id = one_task(&e, task("Fix the flaky e2e test"));
    let again = e
        .add_tasks(vec![task("fix flaky e2e test")])
        .unwrap()
        .pop()
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.matched.unwrap().id, id);
    // Closed work doesn't block re-adding it.
    e.update_task(
        &id,
        crate::tasks::TaskPatch {
            status: Some(TaskStatus::Done),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(e.add_tasks(vec![task("fix flaky e2e test")]).unwrap()[0].created);
}

#[test]
fn progress_appends_dated_lines_and_idle_counts_sessions() {
    let mut e = engine();
    let id = one_task(
        &e,
        crate::tasks::NewTask {
            body: Some("- [ ] step one".into()),
            ..task("Long task")
        },
    );
    let t = e
        .update_task(
            &id,
            crate::tasks::TaskPatch {
                progress: Some("stopped after the store drivers".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let body = t.body.unwrap();
    assert!(body.starts_with("- [ ] step one\n- 20"), "{body}");
    assert!(
        body.ends_with(": stopped after the store drivers"),
        "{body}"
    );

    // Five later sessions brief without touching it: idle.
    let mut cfg = e.graph_config();
    cfg.brief.tasks.idle_sessions = 3;
    e.set_graph_config(&cfg).unwrap();
    let mut t = e.get_task(&id).unwrap().unwrap();
    t.updated_at -= 100;
    e.store().upsert_task(&t).unwrap();
    for s in ["s-a", "s-b", "s-c"] {
        e.set_audit_origin(AuditOrigin {
            origin: "mcp".into(),
            session_id: Some(s.into()),
        });
        e.audit_activity("brief_served", None).unwrap();
    }
    e.set_audit_origin(AuditOrigin {
        origin: "mcp".into(),
        session_id: Some("s-now".into()),
    });
    let brief = e.brief(16_000).unwrap();
    assert!(brief.contains("idle 3 sessions"), "{brief}");
}

#[test]
fn tasks_round_trip_every_store_export_and_sealing() {
    for e in [
        Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ),
        Engine::new(
            TepinStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ),
    ] {
        let id = one_task(
            &e,
            crate::tasks::NewTask {
                body: Some("plan".into()),
                close: Some("2030-01-01".into()),
                ..task("Survive the round trip")
            },
        );
        let t = e.get_task(&id).unwrap().unwrap();
        assert_eq!(t.close.as_deref(), Some("2030-01-01T00:00:00Z"));
        let dump = e.export().unwrap();
        assert_eq!(dump.tasks.len(), 1);
        let json = serde_json::to_string(&dump).unwrap();
        let back = Engine::new(
            TepinStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        back.import(serde_json::from_str(&json).unwrap()).unwrap();
        assert_eq!(back.get_task(&id).unwrap().unwrap(), t);
        assert!(e.delete_task(&id).unwrap());
        assert!(e.get_task(&id).unwrap().is_none());
    }

    let e = sealed_capable_engine();
    let id = one_task(
        &e,
        crate::tasks::NewTask {
            body: Some("secret plan".into()),
            ..task("Sealed title")
        },
    );
    e.set_store_encryption("graph", true, &mut |_, _| {})
        .unwrap();
    let t = e.get_task(&id).unwrap().unwrap();
    assert_eq!(
        (t.title.as_str(), t.body.as_deref()),
        ("Sealed title", Some("secret plan"))
    );
    e.update_task(
        &id,
        crate::tasks::TaskPatch {
            title: Some("Sealed title, renamed".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(e.tasks().unwrap()[0].title, "Sealed title, renamed");
}

#[test]
fn tasks_can_be_turned_off_and_back_on() {
    let e = engine();
    let id = one_task(&e, task("Kept while the feature is off"));
    let mut cfg = e.graph_config();
    cfg.tasks.enabled = false;
    e.set_graph_config(&cfg).unwrap();

    assert!(!e.brief(16_000).unwrap().contains("## Tasks"));
    let refused = e.add_tasks(vec![task("Nope")]).unwrap_err().to_string();
    assert!(
        refused.contains("turned off") && refused.contains("handoff"),
        "{refused}"
    );
    assert!(e.list_tasks(&TaskFilter::default()).is_err());
    assert!(e.update_task(&id, Default::default()).is_err());
    assert!(e.tasks().unwrap().is_empty(), "reporting reads go quiet");
    assert_eq!(
        e.store().all_tasks().unwrap().len(),
        1,
        "stored tasks are kept"
    );

    cfg.tasks.enabled = true;
    e.set_graph_config(&cfg).unwrap();
    assert!(
        e.brief(16_000)
            .unwrap()
            .contains("Kept while the feature is off")
    );
}
