//! Session-diverse delivery.

#[test]
fn session_diverse_select_with_zero_demote_is_truncate() {
    let mut hits: Vec<(usize, Option<&str>)> =
        (0..10).map(|i| (i, Some(["a", "b"][i % 2]))).collect();
    let expect: Vec<usize> = (0..5).collect();
    crate::engine::session_diverse_select(&mut hits, 5, 0.0, |h| h.1);
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), expect);
}

#[test]
fn session_diverse_select_lets_other_sessions_into_the_cut() {
    // Four hits from session "a" head the list; the demotion should hand the
    // later slots to "b" and "c" instead of a's third and fourth restatement.
    let sessions = ["a", "a", "a", "a", "b", "c"];
    let mut hits: Vec<(usize, Option<&str>)> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| (i, Some(*s)))
        .collect();
    crate::engine::session_diverse_select(&mut hits, 4, 3.0, |h| h.1);
    let picked: Vec<usize> = hits.iter().map(|h| h.0).collect();
    // 0 first (rank 0). 1 costs 1+3=4, so 4 ("b", rank 4) and 5 ("c") tie
    // against it: 1 beats 4 (4 == 4 keeps the earlier index), then "a"'s
    // second repeat costs 2+6=8 and both fresh sessions win the last slots.
    assert_eq!(picked, vec![0, 1, 4, 5]);
}

#[test]
fn session_diverse_select_never_demotes_sessionless_hits() {
    let mut hits: Vec<(usize, Option<&str>)> = vec![
        (0, Some("a")),
        (1, None),
        (2, None),
        (3, Some("a")),
        (4, Some("b")),
    ];
    crate::engine::session_diverse_select(&mut hits, 3, 10.0, |h| h.1);
    let picked: Vec<usize> = hits.iter().map(|h| h.0).collect();
    // The sessionless pair keeps its rank; only a's second hit is demoted.
    assert_eq!(picked, vec![0, 1, 2]);
}

#[test]
fn session_diverse_select_pool_shallower_than_limit_is_identity() {
    let mut hits: Vec<(usize, Option<&str>)> = vec![(0, Some("a")), (1, Some("a"))];
    crate::engine::session_diverse_select(&mut hits, 5, 4.0, |h| h.1);
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![0, 1]);
}

#[test]
fn session_diversity_demote_is_validated() {
    let mut cfg = crate::config::GraphConfig::default();
    cfg.policy.session_diversity_demote = -1.0;
    assert!(cfg.validate().is_err());
    cfg.policy.session_diversity_demote = 51.0;
    assert!(cfg.validate().is_err());
    cfg.policy.session_diversity_demote = 3.0;
    assert!(cfg.validate().is_ok());
}
