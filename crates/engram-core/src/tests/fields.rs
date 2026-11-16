//! Custom fields and the date-field event clock.

use super::*;

pub(super) fn fdef(name: &str, kind: crate::config::FieldKind) -> crate::config::FieldDef {
    crate::config::FieldDef {
        name: name.into(),
        label: String::new(),
        kind,
        values: vec![],
        required: false,
        applies_to: vec![],
        indexed: false,
        show_in_brief: false,
    }
}

/// A config declaring one field of every kind, with `priority` required on
/// Decisions only.
pub(super) fn fields_config() -> GraphConfig {
    use crate::config::FieldKind::*;
    GraphConfig {
        fields: vec![
            crate::config::FieldDef {
                values: vec!["low".into(), "high".into()],
                required: true,
                applies_to: vec!["Decision".into()],
                ..fdef("priority", Enum)
            },
            fdef("effort_days", Number),
            fdef("reviewed", Bool),
            fdef("due", Date),
            fdef("ticket", Url),
            fdef("owner", Text),
        ],
        ..GraphConfig::default()
    }
}

pub(super) fn jmap(
    pairs: &[(&str, serde_json::Value)],
) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn every_reserved_field_name_is_refused() {
    for name in crate::config::RESERVED_FIELD_NAMES {
        let cfg = GraphConfig {
            fields: vec![fdef(name, crate::config::FieldKind::Text)],
            ..GraphConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("shadows a built-in"),
            "{name} must be reserved, got: {err}"
        );
    }
}

#[test]
fn field_def_shapes_are_validated() {
    use crate::config::FieldKind::*;
    let cases: Vec<(crate::config::FieldDef, &str)> = vec![
        (fdef("BadCase", Text), "snake_case"),
        (fdef("9starts_digit", Text), "snake_case"),
        (fdef("has-dash", Text), "snake_case"),
        (fdef("", Text), "snake_case"),
        (fdef(&"x".repeat(33), Text), "snake_case"),
        (fdef("no_values_enum", Enum), "at least one value"),
        (
            crate::config::FieldDef {
                values: vec!["a".into()],
                ..fdef("values_on_text", Text)
            },
            "only makes sense on the enum kind",
        ),
        (
            crate::config::FieldDef {
                applies_to: vec!["NoSuchType".into()],
                ..fdef("bad_target", Text)
            },
            "unknown type",
        ),
        (
            crate::config::FieldDef {
                label: "y".repeat(65),
                ..fdef("long_label", Text)
            },
            "label longer",
        ),
        (
            crate::config::FieldDef {
                values: vec!["a".into(), "a".into()],
                ..fdef("dup_enum", Enum)
            },
            "duplicate enum value",
        ),
    ];
    for (def, needle) in cases {
        let cfg = GraphConfig {
            fields: vec![def],
            ..GraphConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains(needle), "wanted {needle:?} in: {err}");
    }
    // Duplicate names across defs.
    let cfg = GraphConfig {
        fields: vec![fdef("twice", Text), fdef("twice", Number)],
        ..GraphConfig::default()
    };
    assert!(
        cfg.validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate field name")
    );
}

#[test]
fn write_boundary_enforces_kinds_required_and_unknown_names() {
    let e = engine();
    e.set_graph_config(&fields_config()).unwrap();

    // Missing required on the applicable type: a teaching error naming the
    // roster and the call shape.
    let err = e
        .add_node(new_node(NodeType::Decision, "pick a queue", "x"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing required"), "{err}");
    assert!(err.contains("priority"), "{err}");
    assert!(
        err.contains("low | high"),
        "the enum vocabulary teaches: {err}"
    );
    assert!(err.contains("\"fields\""), "the call shape teaches: {err}");

    // The required field doesn't apply to other types.
    let mut n = new_node(NodeType::Insight, "no priority needed", "x");
    n.fields = None;
    e.add_node(n).unwrap();

    // Unknown name, wrong kinds, bad enum value, bad date, bad url.
    let bad: Vec<(serde_json::Map<String, serde_json::Value>, &str)> = vec![
        (
            jmap(&[("nonsense", serde_json::json!("x"))]),
            "unknown custom field",
        ),
        (
            jmap(&[("effort_days", serde_json::json!("three"))]),
            "expected a number",
        ),
        (
            jmap(&[("reviewed", serde_json::json!("yes"))]),
            "expected true or false",
        ),
        (
            jmap(&[("owner", serde_json::json!(7))]),
            "expected a string",
        ),
        (jmap(&[("due", serde_json::json!("soon"))]), "YYYY-MM-DD"),
        (jmap(&[("ticket", serde_json::json!("JIRA-123"))]), "URL"),
    ];
    for (fields, needle) in bad {
        let mut n = new_node(NodeType::Insight, "probe", "x");
        n.fields = Some(fields.clone());
        let err = e.add_node(n).unwrap_err().to_string();
        assert!(
            err.contains(needle),
            "fields {fields:?}: wanted {needle:?} in {err}"
        );
    }
    // Enum vocabulary is enforced (on the type the enum applies to).
    let mut n = new_node(NodeType::Decision, "probe enum", "x");
    n.fields = Some(jmap(&[("priority", serde_json::json!("urgent"))]));
    let err = e.add_node(n).unwrap_err().to_string();
    assert!(err.contains("not one of"), "{err}");

    // A fully valid write persists every kind, and the values read back.
    let mut n = new_node(NodeType::Decision, "adopt the queue", "x");
    n.fields = Some(jmap(&[
        ("priority", serde_json::json!("high")),
        ("effort_days", serde_json::json!(2.5)),
        ("reviewed", serde_json::json!(true)),
        ("due", serde_json::json!("2026-09-15")),
        ("ticket", serde_json::json!("https://issues.example/Q-1")),
        ("owner", serde_json::json!("platform team")),
    ]));
    let node = e.add_node(n).unwrap();
    let stored = e.get_node(&node.id).unwrap().unwrap();
    let f = stored.fields.as_ref().unwrap();
    assert_eq!(f["priority"], "high");
    assert_eq!(f["effort_days"], 2.5);
    assert_eq!(f["reviewed"], true);
    assert_eq!(f["due"], "2026-09-15");
    assert_eq!(f["owner"], "platform team");
    // Unix-seconds dates are legal too.
    let mut n = new_node(NodeType::Insight, "epoch date", "x");
    n.fields = Some(jmap(&[("due", serde_json::json!(1_787_000_000_i64))]));
    e.add_node(n).unwrap();
}

#[test]
fn fields_patch_merges_and_null_deletes() {
    let e = engine();
    e.set_graph_config(&fields_config()).unwrap();
    let mut n = new_node(NodeType::Decision, "merge target", "x");
    n.fields = Some(jmap(&[
        ("priority", serde_json::json!("low")),
        ("owner", serde_json::json!("core team")),
    ]));
    let node = e.add_node(n).unwrap();

    // Merge: touch one key, delete another, leave the rest alone.
    let updated = e
        .update_node(
            &node.id,
            NodePatch {
                fields: Some(jmap(&[
                    ("priority", serde_json::json!("high")),
                    ("owner", serde_json::Value::Null),
                    ("reviewed", serde_json::json!(true)),
                ])),
                ..NodePatch::default()
            },
        )
        .unwrap();
    let f = updated.fields.as_ref().unwrap();
    assert_eq!(f["priority"], "high");
    assert_eq!(f["reviewed"], true);
    assert!(!f.contains_key("owner"), "null deletes the key");

    // Deleting a REQUIRED field is refused — the resolved map must validate.
    let err = e
        .update_node(
            &node.id,
            NodePatch {
                fields: Some(jmap(&[("priority", serde_json::Value::Null)])),
                ..NodePatch::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing required"), "{err}");

    // Retype re-validates against the target type: Insight doesn't carry
    // `priority` (applies_to Decision only) — the stale key must be cleaned
    // in the same patch or the retype is refused.
    let err = e
        .update_node(
            &node.id,
            NodePatch {
                node_type: Some(NodeType::Insight),
                ..NodePatch::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown custom field"), "{err}");
    e.update_node(
        &node.id,
        NodePatch {
            node_type: Some(NodeType::Insight),
            fields: Some(jmap(&[("priority", serde_json::Value::Null)])),
            ..NodePatch::default()
        },
    )
    .unwrap();
}

#[test]
fn fields_without_declarations_are_refused() {
    let e = engine();
    let mut n = new_node(NodeType::Decision, "no defs here", "x");
    n.fields = Some(jmap(&[("anything", serde_json::json!("x"))]));
    let err = e.add_node(n).unwrap_err().to_string();
    assert!(err.contains("declares no custom fields"), "{err}");
    // An empty map is the same as none.
    let mut n = new_node(NodeType::Decision, "empty map fine", "x");
    n.fields = Some(serde_json::Map::new());
    assert!(e.add_node(n).unwrap().fields.is_none());
}

#[test]
fn rename_field_moves_values_and_config() {
    let e = engine();
    e.set_graph_config(&fields_config()).unwrap();
    let mut n = new_node(NodeType::Insight, "carries owner", "x");
    n.fields = Some(jmap(&[("owner", serde_json::json!("infra team"))]));
    let node = e.add_node(n).unwrap();
    let moved = e.rename_field("owner", "steward").unwrap();
    assert_eq!(moved, 1);
    let f = e.get_node(&node.id).unwrap().unwrap().fields.unwrap();
    assert_eq!(f["steward"], "infra team");
    assert!(!f.contains_key("owner"));
    assert!(e.config().field_def("steward").is_some());
    assert!(e.config().field_def("owner").is_none());
    // Renaming onto a reserved name is refused by validation.
    let err = e
        .rename_field("steward", "created_at")
        .unwrap_err()
        .to_string();
    assert!(err.contains("shadows a built-in"), "{err}");
}

#[test]
fn fields_survive_export_import_and_scrub_secrets() {
    let e = engine();
    e.set_graph_config(&fields_config()).unwrap();
    let mut n = new_node(NodeType::Insight, "with secret field", "x");
    n.fields = Some(jmap(&[(
        "owner",
        serde_json::json!("token ghp_1234567890abcdefghijklmnopqrstuvwxyZZZZ"),
    )]));
    let node = e.add_node(n).unwrap();
    let stored = e.get_node(&node.id).unwrap().unwrap();
    let owner = stored.fields.as_ref().unwrap()["owner"].as_str().unwrap();
    assert!(
        owner.contains("[REDACTED]") && !owner.contains("ghp_"),
        "text field values pass redaction: {owner}"
    );
    // Round-trip through export/import keeps the map.
    let dump = e.export().unwrap();
    let e2 = engine();
    e2.set_graph_config(&fields_config()).unwrap();
    e2.import(dump).unwrap();
    let f = e2.get_node(&node.id).unwrap().unwrap().fields.unwrap();
    assert_eq!(f["owner"].as_str().unwrap(), owner);
}

#[test]
fn indexed_fields_reach_keyword_search_on_tepin() {
    use crate::config::FieldKind;
    let e = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let cfg = GraphConfig {
        fields: vec![crate::config::FieldDef {
            indexed: true,
            ..fdef("codename", FieldKind::Text)
        }],
        ..GraphConfig::default()
    };
    e.set_graph_config(&cfg).unwrap();
    let mut n = new_node(
        NodeType::Decision,
        "an unrelated title",
        "an unrelated body",
    );
    n.fields = Some(jmap(&[("codename", serde_json::json!("zanzibar"))]));
    let node = e.add_node(n).unwrap();
    let hits = e.store().search_fts("zanzibar", &[], 5).unwrap();
    assert!(
        hits.iter().any(|h| h.id == node.id),
        "keyword search must find the node through its indexed custom field"
    );
    // Non-indexed fields stay out of the keyword channel.
    let mut cfg2 = fields_config();
    cfg2.fields.push(crate::config::FieldDef {
        indexed: false,
        ..fdef("quiet", FieldKind::Text)
    });
    let e2 = Engine::new(
        TepinStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    e2.set_graph_config(&cfg2).unwrap();
    let mut n = new_node(NodeType::Insight, "plain title", "plain body");
    n.fields = Some(jmap(&[("quiet", serde_json::json!("xylophone"))]));
    let node2 = e2.add_node(n).unwrap();
    let hits = e2.store().search_fts("xylophone", &[], 5).unwrap();
    assert!(
        !hits.iter().any(|h| h.id == node2.id),
        "non-indexed field values must not enter the keyword index"
    );
}

// ---------------------------------------------------------------------------
// 0.9.0: bitemporal search — a date_field clock re-aims the window at the
// graph's event clock (single field, or a from..to validity span).
// ---------------------------------------------------------------------------

#[test]
fn date_field_clock_filters_on_the_event_clock() {
    use crate::config::FieldKind;
    let e = engine();
    let cfg = GraphConfig {
        fields: vec![
            fdef("event_date", FieldKind::Date),
            fdef("effective_from", FieldKind::Date),
            fdef("effective_to", FieldKind::Date),
        ],
        ..GraphConfig::default()
    };
    e.set_graph_config(&cfg).unwrap();

    // Historic import: all captured NOW, but the events span years.
    let mk = |title: &str, fields: &[(&str, serde_json::Value)]| {
        let mut n = new_node(NodeType::Decision, title, "the payment provider migration");
        n.fields = Some(jmap(fields));
        e.add_node(n).unwrap()
    };
    let d2019 = mk(
        "adopted stripe",
        &[("event_date", serde_json::json!("2019-06-01"))],
    );
    let d2023 = mk(
        "moved to adyen",
        &[("event_date", serde_json::json!("2023-02-10"))],
    );
    let no_date = mk("provider decision undated", &[]);

    let filter = e
        .time_filter_clocked(
            Some("2022-01-01"),
            Some("2024-01-01"),
            None,
            None,
            Some("event_date"),
        )
        .unwrap();
    let hits = e
        .search_filtered("payment provider migration", &[], 10, &filter)
        .unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&d2023.id.as_str()), "in-window event matches");
    assert!(
        !ids.contains(&d2019.id.as_str()),
        "out-of-window event drops"
    );
    assert!(
        !ids.contains(&no_date.id.as_str()),
        "a node without the event field can't answer an event-time question"
    );

    // The same window on the DEFAULT clock still matches everything —
    // capture time is now for all three (bitemporal: two clocks, one graph).
    let capture_filter = e
        .time_filter_clocked(Some("yesterday"), None, None, None, None)
        .unwrap();
    let hits = e
        .search_filtered("payment provider migration", &[], 10, &capture_filter)
        .unwrap();
    assert_eq!(hits.len(), 3, "capture clock sees all three: {hits:?}");

    // Validity spans: overlap semantics, open ends included.
    let span = |t: &str, from: Option<&str>, to: Option<&str>| {
        let mut fields: Vec<(&str, serde_json::Value)> = Vec::new();
        if let Some(f) = from {
            fields.push(("effective_from", serde_json::json!(f)));
        }
        if let Some(f) = to {
            fields.push(("effective_to", serde_json::json!(f)));
        }
        mk(t, &fields)
    };
    let ended = span(
        "stripe contract era",
        Some("2019-01-01"),
        Some("2021-12-31"),
    );
    let current = span("adyen contract era", Some("2023-01-01"), None);
    let query_2023 = e
        .time_filter_clocked(
            Some("2023-06-01"),
            Some("2023-06-02"),
            None,
            None,
            Some("effective_from..effective_to"),
        )
        .unwrap();
    let hits = e
        .search_filtered("payment provider migration contract", &[], 10, &query_2023)
        .unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert!(
        ids.contains(&current.id.as_str()),
        "open-ended span overlaps 'valid at' query: {ids:?}"
    );
    assert!(
        !ids.contains(&ended.id.as_str()),
        "a span that ended in 2021 is not valid in 2023"
    );
}

#[test]
fn date_field_clock_is_validated_loudly() {
    use crate::config::FieldKind;
    let e = engine();
    let cfg = GraphConfig {
        fields: vec![
            fdef("event_date", FieldKind::Date),
            fdef("owner", FieldKind::Text),
        ],
        ..GraphConfig::default()
    };
    e.set_graph_config(&cfg).unwrap();
    let probe = |after: Option<&str>, sel: &str| {
        let filter = e
            .time_filter_clocked(after, None, None, None, Some(sel))
            .unwrap();
        e.search_filtered("anything", &[], 5, &filter)
            .unwrap_err()
            .to_string()
    };
    let err = probe(Some("2024-01-01"), "no_such_field");
    assert!(err.contains("event_date"), "names the date roster: {err}");
    let err = probe(Some("2024-01-01"), "owner");
    assert!(
        err.contains("date-kind"),
        "a text field can't be a clock: {err}"
    );
    let err = probe(None, "event_date");
    assert!(
        err.contains("needs a window"),
        "an unaimed clock is an error: {err}"
    );
    let err = e
        .time_filter_clocked(Some("2024-01-01"), None, None, None, Some("a.."))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("from..to"),
        "half a pair is a shape error: {err}"
    );
}
