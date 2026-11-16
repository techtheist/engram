//! Time windows, timespec parsing and claim texts.

use super::*;

/// Every constant below is derived from `parse_day`, which has its own test —
/// hand-written epoch seconds in a test are a second implementation nobody
/// checks. `T0` is 2026-08-14T12:00:00Z: a Friday, mid-month, mid-day, so
/// every period start is strictly in the past and no assertion can pass by
/// landing on a boundary.
pub(super) fn day(s: &str) -> i64 {
    parse_day(s).expect(s)
}

pub(super) fn t0() -> i64 {
    day("2026-08-14") + 12 * 3600
}

#[test]
fn timespec_reads_absolute_instants_days_and_unix_stamps() {
    use crate::timespec::parse_instant;
    let (t0, d) = (t0(), day("2026-08-14"));

    // A full ISO instant keeps its seconds; the same string as a bare day
    // floors to midnight. Both shapes are accepted, and they are not the same
    // instant — a window asked in days must not silently gain nine hours.
    assert_eq!(parse_instant("2026-08-14T09:15:32Z", t0), Some(d + 33332));
    assert_eq!(parse_instant("2026-08-14", t0), Some(d));
    // Offsets shift back to UTC and fractional seconds are ignored, per the
    // harness parser this delegates to.
    assert_eq!(
        parse_instant("2026-08-14T11:15:32+02:00", t0),
        Some(d + 33332)
    );
    // Raw unix seconds pass through, so a caller can echo a timestamp a hit
    // gave it without reformatting.
    assert_eq!(parse_instant(" 1784678400 ", t0), Some(1784678400));
    // Absolute parsing does not consult the anchor at all.
    assert_eq!(
        parse_instant("2026-08-14", t0),
        parse_instant("2026-08-14", 0)
    );
}

#[test]
fn timespec_relative_expressions_resolve_to_span_starts() {
    use crate::timespec::parse_instant;
    let t0 = t0();
    let p = |s: &str| parse_instant(s, t0).expect(s);

    assert_eq!(p("now"), t0);
    assert_eq!(p("today"), day("2026-08-14"), "midnight UTC, not now");
    assert_eq!(p("yesterday"), day("2026-08-13"));
    // Every relative expression is the START of the span it names, so `after:
    // "last week"` reads "since a week ago".
    assert_eq!(p("last week"), t0 - 7 * 86400);
    assert_eq!(p("past week"), t0 - 7 * 86400, "past is a synonym for last");
    assert_eq!(p("last 3 days"), t0 - 3 * 86400);
    assert_eq!(p("2 hours ago"), t0 - 7200);
    assert_eq!(p("a week ago"), t0 - 7 * 86400);
    assert_eq!(p("an hour ago"), t0 - 3600);
    assert_eq!(p("30 minutes ago"), t0 - 1800);
    // Plural and singular are the same unit; so are the common short forms.
    assert_eq!(p("1 day ago"), p("1 days ago"));
    assert_eq!(p("5 min ago"), t0 - 300);

    // Calendar period starts. 2026-08-14 is a Friday, so the week began Monday
    // the 10th — weeks start Monday, the ISO convention.
    assert_eq!(p("this week"), day("2026-08-10"));
    assert_eq!(p("this month"), day("2026-08-01"));
    assert_eq!(p("this year"), day("2026-01-01"));

    // Nothing else is guessed at.
    assert_eq!(parse_instant("soonish", t0), None);
    assert_eq!(parse_instant("", t0), None);
    assert_eq!(parse_instant("last fortnight", t0), None);
}

#[test]
fn timespec_month_and_year_shifts_use_calendar_arithmetic() {
    use crate::timespec::parse_instant;
    let t0 = t0();
    // From 2026-08-14, one month back is 2026-07-14 — 31 days, not an
    // approximate 30. A project's cadence is calendar-shaped, and a fudged
    // month quietly misses a whole release cycle.
    assert_eq!(parse_instant("a month ago", t0).unwrap(), t0 - 31 * 86400);
    assert_eq!(
        parse_instant("6 months ago", t0).unwrap(),
        day("2026-02-14") + 12 * 3600
    );
    assert_eq!(
        parse_instant("a year ago", t0).unwrap(),
        day("2025-08-14") + 12 * 3600
    );
    // Day-of-month clamps into short months instead of overflowing: one month
    // back from the 31st of March is the 28th of February, not the 3rd of March.
    assert_eq!(
        parse_instant("a month ago", day("2026-03-31")).unwrap(),
        day("2026-02-28")
    );
    // Leap years are real years.
    assert_eq!(
        parse_instant("1 month ago", day("2024-03-31")).unwrap(),
        day("2024-02-29")
    );
    // A shift keeps the time of day it started from.
    assert_eq!(
        parse_instant("1 month ago", day("2026-03-15") + 9 * 3600).unwrap(),
        day("2026-02-15") + 9 * 3600
    );
    // An absurd count is unreadable, not a wrapped instant or a panic — the
    // calendar path is checked like the fixed-unit path already was.
    assert_eq!(parse_instant("9223372036854775807 months ago", t0), None);
    assert_eq!(parse_instant("9223372036854775807 years ago", t0), None);
    assert_eq!(parse_instant("9223372036854775807 weeks ago", t0), None);
    assert_eq!(parse_instant("last 99999999 years", t0), None);
    assert!(parse_instant("last 1000 years", t0).is_some());
}

#[test]
fn timespec_windows_are_half_open_and_reject_empty_bounds() {
    use crate::timespec::{TimeWindow, window};

    let t0 = t0();
    let w = window(Some("2026-08-01"), Some("2026-08-14"), t0).unwrap();
    assert!(w.contains(day("2026-08-01")), "after is inclusive");
    assert!(!w.contains(day("2026-08-14")), "before is exclusive");
    assert!(!w.contains(day("2026-08-01") - 1));
    assert!(w.contains(day("2026-08-13")));

    assert!(TimeWindow::default().is_open());
    assert!(
        TimeWindow::default().contains(0),
        "an open window filters nothing"
    );
    // Half-open is what lets adjacent windows tile: the boundary instant
    // belongs to exactly one of them.
    let earlier = window(Some("2026-08-01"), Some("2026-08-10"), t0).unwrap();
    let later = window(Some("2026-08-10"), None, t0).unwrap();
    let boundary = day("2026-08-10");
    assert!(!earlier.contains(boundary) && later.contains(boundary));

    // A bound that cannot be read is an error naming the offender — never a
    // dropped filter, which would answer an unscoped question instead.
    let err = window(Some("whenever"), None, t0).unwrap_err().to_string();
    assert!(err.contains("whenever"), "{err}");
    assert!(
        err.contains("last week"),
        "the error teaches the grammar: {err}"
    );

    // Crossed bounds are refused: silently returning nothing is
    // indistinguishable from "the graph is silent".
    let err = window(Some("2026-08-14"), Some("2026-08-01"), t0)
        .unwrap_err()
        .to_string();
    assert!(err.contains("empty"), "{err}");

    // Blank strings are absent bounds, not unreadable ones — an omitted
    // optional arriving as "" over JSON must not become an error.
    assert!(window(Some(""), Some("  "), t0).unwrap().is_open());
}

#[test]
fn timespec_windows_intersect_to_the_tighter_bound() {
    use crate::timespec::TimeWindow;
    let release = TimeWindow {
        after: Some(100),
        before: Some(200),
    };
    let asked = TimeWindow {
        after: Some(150),
        before: None,
    };
    let both = release.intersect(asked);
    assert_eq!(both.after, Some(150), "the later start wins");
    assert_eq!(both.before, Some(200), "the earlier end wins");
    assert!(!both.contains(120));
    assert!(both.contains(150));
}

#[test]
fn search_order_parses_its_aliases_and_nothing_else() {
    use crate::timespec::SearchOrder;
    assert_eq!(SearchOrder::default(), SearchOrder::Relevance);
    assert_eq!(SearchOrder::parse("recent"), Some(SearchOrder::Recent));
    assert_eq!(SearchOrder::parse("NEWEST"), Some(SearchOrder::Recent));
    assert_eq!(
        SearchOrder::parse("chronological"),
        Some(SearchOrder::Chronological)
    );
    assert_eq!(
        SearchOrder::parse("oldest"),
        Some(SearchOrder::Chronological)
    );
    assert_eq!(
        SearchOrder::parse("relevance"),
        Some(SearchOrder::Relevance)
    );
    assert_eq!(SearchOrder::parse("sideways"), None);
    assert!(!SearchOrder::Relevance.is_temporal());
    assert!(SearchOrder::Recent.is_temporal());
}

/// Add a node dated at `day`, so a window has something to select on.
pub(super) fn dated_note(e: &Engine, title: &str, body: &str, day_str: &str) -> Node {
    let mut n = new_node(NodeType::Decision, title, body);
    n.created_at = Some(day(day_str));
    e.add_node(n).unwrap()
}

#[test]
fn search_window_filters_curated_hits_by_capture_date() {
    let e = engine();
    let june = dated_note(
        &e,
        "sqlite storage decision",
        "we chose sqlite",
        "2026-06-10",
    );
    let august = dated_note(
        &e,
        "sqlite storage revisited",
        "we chose sqlite",
        "2026-08-10",
    );

    // Unfiltered, both are reachable.
    let all = e.search("sqlite storage", &[], 10).unwrap();
    assert_eq!(all.len(), 2);

    let only_august = e
        .search_filtered(
            "sqlite storage",
            &[],
            10,
            &SearchFilter {
                window: crate::timespec::window(Some("2026-08-01"), None, day("2026-08-14"))
                    .unwrap(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(only_august.len(), 1);
    assert_eq!(only_august[0].id, august.id);

    let only_june = e
        .search_filtered(
            "sqlite storage",
            &[],
            10,
            &SearchFilter {
                window: crate::timespec::window(None, Some("2026-08-01"), day("2026-08-14"))
                    .unwrap(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(only_june.len(), 1);
    assert_eq!(only_june[0].id, june.id);

    // A window that selects nothing returns nothing — and that is a real
    // "none", not a silent drop of the filter.
    let none = e
        .search_filtered(
            "sqlite storage",
            &[],
            10,
            &SearchFilter {
                window: crate::timespec::window(Some("2020-01-01"), Some("2020-02-01"), 0).unwrap(),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(none.is_empty());
}

/// Found by the 0.8.7 window bench, and it was live in the shipped path: the
/// windowed candidate pool multiplies the vector k, sqlite-vec REFUSES a k
/// over 4096 instead of capping it, and an error there empties the entire
/// search. A caller asking for a large `limit` next to a time window got
/// nothing back — which is indistinguishable from "the graph is silent".
#[test]
fn a_deep_windowed_pool_never_outgrows_the_vector_index() {
    let e = engine();
    for i in 0..12 {
        dated_note(
            &e,
            &format!("sqlite storage note {i}"),
            "we chose sqlite",
            "2026-06-10",
        );
    }
    let mut cfg = e.graph_config();
    // Far past anything shipped, so the clamp is what is under test rather
    // than the default happening to fit.
    cfg.policy.window_overfetch = 64;
    e.set_graph_config(&cfg).unwrap();

    let filter = SearchFilter {
        window: crate::timespec::window(Some("2026-01-01"), None, day("2026-12-01")).unwrap(),
        ..Default::default()
    };
    // A large limit is what pushed k over the ceiling.
    for limit in [1, 8, 25, 100] {
        let hits = e
            .search_filtered("sqlite storage", &[], limit, &filter)
            .unwrap_or_else(|err| panic!("limit {limit} must not error: {err}"));
        assert!(!hits.is_empty(), "limit {limit} returned nothing");
    }
}

#[test]
fn search_hits_carry_their_capture_date_and_can_be_time_ordered() {
    let e = engine();
    let old = dated_note(&e, "sqlite one", "sqlite storage", "2026-06-10");
    let mid = dated_note(&e, "sqlite two", "sqlite storage", "2026-07-10");
    let new = dated_note(&e, "sqlite three", "sqlite storage", "2026-08-10");

    let by_date = |order| {
        e.search_filtered(
            "sqlite storage",
            &[],
            10,
            &SearchFilter {
                order,
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect::<Vec<_>>()
    };

    assert_eq!(
        by_date(crate::timespec::SearchOrder::Chronological),
        vec![old.id.clone(), mid.id.clone(), new.id.clone()]
    );
    assert_eq!(
        by_date(crate::timespec::SearchOrder::Recent),
        vec![new.id.clone(), mid.id.clone(), old.id.clone()]
    );
    // Ordering changes the ORDER, never the membership: the same set comes
    // back, so the verdict a caller was given still describes it.
    let mut relevance = by_date(crate::timespec::SearchOrder::Relevance);
    let mut chrono = by_date(crate::timespec::SearchOrder::Chronological);
    relevance.sort();
    chrono.sort();
    assert_eq!(relevance, chrono);

    // The date rides on the hit, so a caller can order or display without a
    // second read.
    let hits = e.search("sqlite storage", &[], 10).unwrap();
    let dated: Vec<i64> = hits.iter().map(|h| h.created_at).collect();
    assert!(dated.iter().all(|d| *d > 0));
    assert!(dated.contains(&day("2026-06-10")));
}

#[test]
fn during_version_resolves_from_the_recorded_version_switches() {
    let e = engine();
    // A cycle's worth of switches, journaled exactly as set_version writes them.
    e.set_current_version(Some("0.8.5")).unwrap();
    let opened_86 = crate::store::now();
    e.set_current_version(Some("0.8.6")).unwrap();
    e.set_current_version(Some("0.8.7")).unwrap();

    let w = e
        .version_window("0.8.6")
        .unwrap()
        .expect("0.8.6 was worked");
    assert!(w.after.is_some() && w.before.is_some(), "a closed cycle");
    assert!(w.after.unwrap() >= opened_86);
    assert!(w.before.unwrap() >= w.after.unwrap());

    // The current version is still open-ended.
    let current = e.version_window("0.8.7").unwrap().unwrap();
    assert!(current.after.is_some());
    assert_eq!(current.before, None, "the open cycle has no end");

    // The first version has no switch INTO it recorded from before tracking
    // began, but the switch away still bounds it.
    let first = e.version_window("0.8.5").unwrap().unwrap();
    assert!(first.before.is_some());

    // A version this graph never worked under is an error, not an unscoped
    // answer.
    assert!(e.version_window("9.9.9").unwrap().is_none());
    let err = e
        .time_filter(None, None, Some("9.9.9"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("9.9.9"), "{err}");
    assert!(
        err.contains("set_version"),
        "the error names the fix: {err}"
    );
}

#[test]
fn time_filter_intersects_version_window_with_explicit_bounds() {
    let e = engine();
    e.set_current_version(Some("0.8.6")).unwrap();
    e.set_current_version(Some("0.8.7")).unwrap();
    let open = e.version_window("0.8.7").unwrap().unwrap().after.unwrap();

    // An explicit `after` INSIDE the release narrows it.
    let f = e
        .time_filter(Some("2020-01-01"), None, Some("0.8.7"), None)
        .unwrap();
    assert_eq!(
        f.window.after,
        Some(open),
        "the release's later start wins over an earlier explicit one"
    );

    // An explicit window entirely outside the release is refused rather than
    // silently returning nothing.
    let err = e
        .time_filter(None, Some("2020-01-01"), Some("0.8.7"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("empty"), "{err}");

    // Order parses through the same entry point, and a bad one is named.
    assert_eq!(
        e.time_filter(None, None, None, Some("recent"))
            .unwrap()
            .order,
        crate::timespec::SearchOrder::Recent
    );
    let err = e
        .time_filter(None, None, None, Some("sideways"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("chronological"), "{err}");
}

#[test]
fn default_filter_leaves_the_unscoped_path_alone() {
    let e = engine();
    dated_note(
        &e,
        "sqlite storage decision",
        "we chose sqlite",
        "2026-06-10",
    );
    assert!(SearchFilter::default().is_default());
    assert_eq!(
        e.search("sqlite", &[], 5).unwrap().len(),
        e.search_filtered("sqlite", &[], 5, &SearchFilter::default())
            .unwrap()
            .len()
    );
}

#[test]
fn writes_carry_their_original_date_on_both_backends() {
    let stores: Vec<Box<dyn Store>> = vec![
        Box::new(SqliteStore::open_in_memory().unwrap()),
        Box::new(TepinStore::open_in_memory().unwrap()),
    ];
    for s in &stores {
        let mut spec = new_node(NodeType::Decision, "a decision from the past", "");
        spec.created_at = parse_day("2025-03-10");
        let n = s.add_node(spec).unwrap();
        assert_eq!(n.created_at, parse_day("2025-03-10").unwrap());
        assert_eq!(n.valid_from, parse_day("2025-03-10"));

        // The future is clamped away — no recency boost for sale.
        let mut spec = new_node(NodeType::Insight, "a note from tomorrow", "");
        spec.created_at = Some(now() + 7 * 24 * 3600);
        let n = s.add_node(spec).unwrap();
        assert!(n.created_at <= now());

        // Omitted stays exactly as before: dated now.
        let n = s
            .add_node(new_node(NodeType::Caution, "a live capture", ""))
            .unwrap();
        assert!((now() - n.created_at) < 5);
    }
}

// ---- claim-level search (v0.6.3): rich nodes, per-claim vectors ------------

#[test]
fn claim_texts_tokenizes_substantial_sentences_only() {
    use crate::engine_claims_for_tests as claims;
    let body = "The first substantial sentence carries a real claim here. Ok. \
                A second long sentence about an entirely different matter follows; \
                and a third one closes the paragraph with more concrete detail.";
    let out = claims("My node title", Some(body));
    assert!(
        out.len() >= 2,
        "multiple substantial sentences become claims: {out:?}"
    );
    assert!(
        out.iter().all(|c| c.starts_with("My node title. ")),
        "claims keep their subject"
    );
    assert!(
        out.iter().all(|c| !c.contains("Ok.")),
        "fragments are dropped"
    );
    assert!(
        claims(
            "t",
            Some("One sentence only, however long it happens to be written out.")
        )
        .is_empty(),
        "a single claim adds nothing over the composition vector"
    );
    assert!(claims("t", None).is_empty());
}

#[test]
fn buried_claims_are_reachable_on_both_backends() {
    let emb = FakeEmbedder::default();
    let stores: Vec<Box<dyn Store>> = vec![
        Box::new(SqliteStore::open_in_memory().unwrap()),
        Box::new(TepinStore::open_in_memory().unwrap()),
    ];
    for s in &stores {
        // A rich node whose third sentence is the interesting claim, and a
        // decoy that resembles the rich node's OVERALL average text.
        let title = "storage decision";
        let body = "We considered several options for the backend layer overall. \
                    The team benchmarked writes across a few candidate engines. \
                    Zanzibar quorum replication was rejected for latency reasons entirely.";
        let rich = s
            .add_node(new_node(NodeType::Decision, title, body))
            .unwrap();
        let decoy = s
            .add_node(new_node(
                NodeType::Insight,
                "backend layer benchmarks considered",
                "",
            ))
            .unwrap();

        // Store vectors the way the engine does: composition first, claims after.
        let mut texts = vec![format!("{title}\n{body}")];
        texts.extend(crate::engine_claims_for_tests(title, Some(body)));
        assert!(texts.len() > 2, "the rich body yields claim chunks");
        s.upsert_embeddings(&rich.id, &emb.embed(&texts).unwrap())
            .unwrap();
        s.upsert_embeddings(
            &decoy.id,
            &[emb
                .embed_one("backend layer benchmarks considered")
                .unwrap()],
        )
        .unwrap();

        // Query = the buried claim's chunk text: with claim vectors this is a
        // near-exact match (distance ≈ 0); the node-level vector alone would
        // sit much further away.
        let query = emb
            .embed_one(&format!(
                "{title}. Zanzibar quorum replication was rejected for latency reasons entirely."
            ))
            .unwrap();
        let hits = s.search_vec(&query, 2).unwrap();
        assert_eq!(hits[0].0, rich.id, "the buried claim finds its node");
        assert!(
            hits[0].1 < 1e-6,
            "claim chunk matches near-exactly: {}",
            hits[0].1
        );
        assert!(
            hits.iter().all(|(id, _)| !id.contains('#')),
            "chunk keys never leak"
        );

        // embedding_of still answers with the node-level vector.
        let node_vec = s.embedding_of(&rich.id).unwrap().unwrap();
        assert_eq!(
            node_vec,
            emb.embed_one(&format!("{title}\n{body}")).unwrap()
        );
    }
}

#[test]
fn impossible_calendar_dates_are_refused_everywhere() {
    assert!(crate::parse_day("2026-02-28").is_some());
    assert!(crate::parse_day("2024-02-29").is_some(), "leap day");
    for bad in [
        "2026-02-29",
        "2026-02-31",
        "2026-04-31",
        "2026-13-01",
        "2026-00-10",
    ] {
        assert_eq!(crate::parse_day(bad), None, "{bad}");
    }
    let iso = crate::harvest::parse_iso8601;
    assert!(iso("2026-08-11T09:15:32Z").is_some());
    for bad in [
        "2026-02-30T09:15:32Z",
        "2026-08-11T24:00:00Z",
        "2026-08-11T-1:15:32Z",
        "2026-08-11T09:61:00Z",
    ] {
        assert_eq!(iso(bad), None, "{bad}");
    }
}
