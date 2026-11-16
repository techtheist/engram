//! Trust curves: anchors, evidence, exposure.

use super::*;

/// Baseline TrustInputs: unconfirmed, unapproved, undemoted, unpinned episodic.
pub(super) fn ti(created_at: i64) -> policy::TrustInputs {
    policy::TrustInputs {
        created_at,
        confirmed_at: None,
        approved_at: None,
        demoted_at: None,
        trust_override: None,
        durability: Durability::Episodic,
        status: None,
    }
}

#[test]
fn trust_curves_follow_the_anchor_timestamps() {
    use crate::policy::{self, trust};
    let day = 24 * 60 * 60;
    let t0 = 1_000_000i64;

    // created-only episodic: 50% now, linear to the floor at half a year
    assert!((trust(&ti(t0), t0, &dp()) - policy::TRUST_UNSEEN_START).abs() < 1e-9);
    let half_window = t0 + policy::PROVISIONAL_TRUST_WINDOW_SECS / 2;
    let mid = trust(&ti(t0), half_window, &dp());
    assert!((mid - (policy::TRUST_UNSEEN_START + policy::TRUST_FLOOR) / 2.0).abs() < 1e-6);
    assert!(
        (trust(
            &ti(t0),
            t0 + policy::PROVISIONAL_TRUST_WINDOW_SECS + day,
            &dp()
        ) - policy::TRUST_FLOOR)
            .abs()
            < 1e-9
    );

    // confirmed: restarts at 60% from confirmed_at, beats created-only
    let confirmed = policy::TrustInputs {
        confirmed_at: Some(t0 + 100 * day),
        ..ti(t0)
    };
    assert!(
        (trust(&confirmed, t0 + 100 * day, &dp()) - policy::TRUST_CONFIRMED_START).abs() < 1e-9
    );
    assert!(trust(&confirmed, t0 + 100 * day, &dp()) > trust(&ti(t0), t0 + 100 * day, &dp()));

    // approved: 100% at approval, floor 20% past a year, wins over confirmed
    let approved = policy::TrustInputs {
        approved_at: Some(t0),
        confirmed_at: Some(t0),
        ..ti(t0)
    };
    assert!((trust(&approved, t0, &dp()) - policy::TRUST_APPROVED_START).abs() < 1e-9);
    let old = trust(
        &approved,
        t0 + policy::APPROVED_TRUST_WINDOW_SECS + day,
        &dp(),
    );
    assert!((old - policy::TRUST_APPROVED_FLOOR).abs() < 1e-9);

    // volatile rots on the short window
    let volatile = policy::TrustInputs {
        durability: Durability::Volatile,
        ..ti(t0)
    };
    let aged = t0 + policy::VOLATILE_TRUST_WINDOW_SECS + day;
    assert!((trust(&volatile, aged, &dp()) - policy::TRUST_FLOOR).abs() < 1e-9);
    assert!(
        trust(&ti(t0), aged, &dp()) > policy::TRUST_FLOOR,
        "episodic outlives volatile"
    );

    // staleness threshold
    assert!(policy::is_stale(policy::STALE_TRUST - 0.01, &dp()));
    assert!(!policy::is_stale(policy::STALE_TRUST, &dp()));
}

#[test]
fn stable_trust_holds_flat_until_evidence_demotes_it() {
    use crate::policy::{self, trust};
    let year = 365 * 24 * 60 * 60;
    let t0 = 1_000_000i64;
    let stable = policy::TrustInputs {
        durability: Durability::Stable,
        ..ti(t0)
    };

    // Time alone never moves stable knowledge — the redditor's rare
    // production constraint survives its quiet year at full anchor value.
    assert_eq!(
        trust(&stable, t0 + 3 * year, &dp()),
        policy::TRUST_UNSEEN_START
    );
    let approved_stable = policy::TrustInputs {
        approved_at: Some(t0),
        ..stable
    };
    assert_eq!(
        trust(&approved_stable, t0 + 3 * year, &dp()),
        policy::TRUST_APPROVED_START
    );

    // Contradicting evidence starts the ramp — from the event, not creation.
    let demoted = policy::TrustInputs {
        demoted_at: Some(t0 + 2 * year),
        ..stable
    };
    assert_eq!(
        trust(&demoted, t0 + 2 * year, &dp()),
        policy::TRUST_UNSEEN_START
    );
    assert!(
        trust(
            &demoted,
            t0 + 2 * year + policy::PROVISIONAL_TRUST_WINDOW_SECS,
            &dp()
        ) < 0.05
    );

    // Pin overrides everything, including demotion.
    let pinned = policy::TrustInputs {
        trust_override: Some(1.0),
        ..demoted
    };
    assert_eq!(trust(&pinned, t0 + 3 * year, &dp()), 1.0);
    // Constant overrides are clamped to 0..=1.
    let odd = policy::TrustInputs {
        trust_override: Some(1.7),
        ..stable
    };
    assert_eq!(trust(&odd, t0, &dp()), 1.0);

    // Open worklist items are never buried by age.
    let open = policy::TrustInputs {
        status: Some(NodeStatus::Open),
        ..ti(t0)
    };
    assert_eq!(
        trust(&open, t0 + 3 * year, &dp()),
        policy::TRUST_UNSEEN_START
    );
    assert!(
        policy::stale_since(&open, &dp()).is_none(),
        "open never decays out"
    );
    assert!(
        policy::stale_since(&stable, &dp()).is_none(),
        "stable never decays out"
    );
    assert!(
        policy::stale_since(&ti(t0), &dp()).is_some(),
        "plain episodic still crosses"
    );
}

/// The sandbox tester's adversarial scenario: a frequently retrieved false
/// note vs a rarely retrieved true constraint. Exposure must not preserve the
/// false one — retrieval never refreshes trust — while the stable constraint
/// holds without ever being surfaced.
#[test]
fn exposure_does_not_preserve_the_false_note() {
    let e = engine();
    let false_note = e
        .add_node(episodic(NodeType::Insight, "attractive but wrong claim"))
        .unwrap();
    let constraint = e
        .add_node(new_node(
            NodeType::Caution,
            "rare but true production constraint",
            "only matters during the yearly migration",
        ))
        .unwrap();

    // Age both past the episodic stale crossing + decay TTL.
    backdate(&e, &false_note.id, 100);
    backdate(&e, &constraint.id, 100);

    // Broad recurring searches keep surfacing the false note...
    for _ in 0..5 {
        let hits = e.search("attractive wrong claim", &[], 5).unwrap();
        assert!(hits.iter().any(|h| h.id == false_note.id));
    }
    let surfaced = e.get_node(&false_note.id).unwrap().unwrap();
    assert!(
        surfaced.last_seen.is_some(),
        "retrieval is still observable"
    );
    assert!(
        surfaced.confirmed_at.is_none(),
        "…but being findable confirms nothing"
    );
    assert!(surfaced.stale, "exposure did not keep the false note alive");

    // The decay pass takes the exposed false note and spares the quiet truth.
    let archived = e.decay(policy::DECAY_TTL_DAYS, false).unwrap();
    assert_eq!(archived, vec![false_note.id.clone()]);
    let kept = e.get_node(&constraint.id).unwrap().unwrap();
    assert!(kept.valid_until.is_none());
    assert_eq!(kept.trust, policy::TRUST_UNSEEN_START, "stable holds flat");
}

/// The sandbox tester's negative control, verbatim (v0.4.1 feedback thread):
/// two unapproved nodes with equal initial trust and age; retrieve A a
/// hundred times and never B; approve, update, or verify neither; advance
/// the clock — A may RANK higher for its query, but its TRUST must equal
/// B's. Retrieval frequency is telemetry, never evidence quality.
#[test]
fn retrieval_frequency_is_not_evidence() {
    let e = engine();
    let a = e
        .add_node(episodic(NodeType::Insight, "cache warming strategy alpha"))
        .unwrap();
    let b = e
        .add_node(episodic(
            NodeType::Insight,
            "database vacuum schedule omega",
        ))
        .unwrap();
    // Equal age, past enough of the ramp that a refreshed clock would show.
    backdate(&e, &a.id, 60);
    backdate(&e, &b.id, 60);

    // 100 retrievals of A (touch is the exact stamp every search hit and
    // brief inclusion applies), zero of B.
    for _ in 0..100 {
        e.store().touch(std::slice::from_ref(&a.id)).unwrap();
    }

    let a2 = e.get_node(&a.id).unwrap().unwrap();
    let b2 = e.get_node(&b.id).unwrap().unwrap();
    assert!(a2.last_seen.is_some(), "exposure stays observable");
    assert!(b2.last_seen.is_none());
    assert!(
        (a2.trust - b2.trust).abs() < 1e-6,
        "equal trust despite 100:0 exposure — got {} vs {}",
        a2.trust,
        b2.trust
    );
}
