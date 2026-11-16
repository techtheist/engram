//! Validation of the hard invariants every configuration must keep.

use super::*;

impl GraphConfig {
    /// Every invariant that must hold across ANY preset or edit. Returns the
    /// first violation as the error message a 400 carries.
    pub fn validate(&self) -> crate::Result<()> {
        let fail = |msg: String| Err(crate::Error::Config(msg));

        let types = &self.ontology.types;
        if types.is_empty() {
            return fail("ontology needs at least one node type".into());
        }
        let mut seen = std::collections::HashSet::new();
        for t in types {
            let name = t.name.trim();
            if name.is_empty() || name.len() > 32 {
                return fail(format!("type name {:?} must be 1..=32 chars", t.name));
            }
            if name != t.name {
                return fail(format!("type name {:?} has surrounding whitespace", t.name));
            }
            if !seen.insert(name.to_lowercase()) {
                return fail(format!("duplicate type name {name:?}"));
            }
            if t.hue >= 360 {
                return fail(format!("type {name}: hue {} out of 0..360", t.hue));
            }
            if !(0.0..=0.5).contains(&t.roles.rank_prior) {
                return fail(format!(
                    "type {name}: rank_prior {} out of 0..=0.5",
                    t.roles.rank_prior
                ));
            }
            // A tombstone is a closed record of a removal — it can't be open
            // work, and it isn't a live code subject.
            if t.roles.tombstone && (t.roles.worklist || t.roles.anchor) {
                return fail(format!(
                    "type {name}: tombstone can't also carry worklist or anchor"
                ));
            }
            validate_section(&t.brief, &format!("type {name} brief"))?;
        }

        let verbs = &self.ontology.verbs;
        let mut seen = std::collections::HashSet::new();
        for v in verbs {
            // Sentence-shaped: a lowercase hyphen-joined verb phrase, so any
            // triple reads as English ("X conflicts-with Y").
            if v.name.is_empty()
                || v.name.len() > 32
                || !v
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                || v.name.starts_with('-')
                || v.name.ends_with('-')
            {
                return fail(format!(
                    "verb {:?} must be a lowercase hyphen-joined word (sentence-shaped)",
                    v.name
                ));
            }
            if !seen.insert(v.name.clone()) {
                return fail(format!("duplicate verb {:?}", v.name));
            }
        }
        for (role, count) in [
            (
                "supersession",
                verbs.iter().filter(|v| v.roles.supersession).count(),
            ),
            (
                "contradiction",
                verbs.iter().filter(|v| v.roles.contradiction).count(),
            ),
        ] {
            if count != 1 {
                return fail(format!(
                    "exactly one verb must carry the {role} role (found {count}) — it is what keeps the graph active"
                ));
            }
        }
        if let Some(v) = verbs
            .iter()
            .find(|v| v.roles.supersession && v.roles.contradiction)
        {
            return fail(format!(
                "verb {:?} can't be both supersession and contradiction",
                v.name
            ));
        }

        // Custom fields: names must be usable as JSON keys everywhere
        // a node travels, must never shadow a built-in field, and enum kinds
        // must actually enumerate something.
        let mut seen = std::collections::HashSet::new();
        for f in &self.fields {
            let name = f.name.as_str();
            if name.is_empty()
                || name.len() > 32
                || !name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                || !name.starts_with(|c: char| c.is_ascii_lowercase())
            {
                return fail(format!(
                    "field name {name:?} must be lowercase snake_case (1..=32 chars, starting with a letter)"
                ));
            }
            if RESERVED_FIELD_NAMES.contains(&name) {
                return fail(format!(
                    "field name {name:?} shadows a built-in node field — reserved names: {}",
                    RESERVED_FIELD_NAMES.join(", ")
                ));
            }
            if !seen.insert(name.to_string()) {
                return fail(format!("duplicate field name {name:?}"));
            }
            if f.label.len() > 64 {
                return fail(format!("field {name}: label longer than 64 chars"));
            }
            match f.kind {
                FieldKind::Enum => {
                    if f.values.is_empty() {
                        return fail(format!("field {name}: enum kind needs at least one value"));
                    }
                    let mut vs = std::collections::HashSet::new();
                    for v in &f.values {
                        if v.is_empty() || v.len() > 200 {
                            return fail(format!(
                                "field {name}: enum value {v:?} out of 1..=200 chars"
                            ));
                        }
                        if !vs.insert(v) {
                            return fail(format!("field {name}: duplicate enum value {v:?}"));
                        }
                    }
                }
                _ => {
                    if !f.values.is_empty() {
                        return fail(format!(
                            "field {name}: `values` only makes sense on the enum kind"
                        ));
                    }
                }
            }
            let mut ats = std::collections::HashSet::new();
            for t in &f.applies_to {
                if self.type_def(t).is_none() {
                    return fail(format!(
                        "field {name}: applies_to names unknown type {t:?} — declared types: {}",
                        types
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if !ats.insert(t) {
                    return fail(format!("field {name}: duplicate applies_to entry {t:?}"));
                }
            }
        }

        let p = &self.policy;
        for (name, value) in [
            ("trust_created", p.trust_created),
            ("trust_confirmed", p.trust_confirmed),
            ("trust_approved", p.trust_approved),
            ("trust_approved_floor", p.trust_approved_floor),
            ("trust_floor", p.trust_floor),
            ("stale_trust", p.stale_trust),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return fail(format!("policy.{name} {value} out of 0..=1"));
            }
        }
        if p.trust_floor > p.trust_created
            || p.trust_created > p.trust_confirmed
            || p.trust_confirmed > p.trust_approved
        {
            return fail(
                "trust anchors must order floor <= created <= confirmed <= approved".into(),
            );
        }
        for (name, days) in [
            ("episodic_window_days", p.episodic_window_days),
            ("volatile_window_days", p.volatile_window_days),
            ("approved_window_days", p.approved_window_days),
            ("decay_ttl_days", p.decay_ttl_days),
        ] {
            if !(1..=36_500).contains(&days) {
                return fail(format!("policy.{name} {days} out of 1..=36500"));
            }
        }
        for (name, value) in [
            ("duplicate_similarity", p.duplicate_similarity),
            ("conflict_suspect_similarity", p.conflict_suspect_similarity),
            ("warn_similarity", p.warn_similarity),
            ("nli_sweep_min_confidence", p.nli_sweep_min_confidence),
            ("keyword_weight", p.keyword_weight),
            ("semantic_floor", p.semantic_floor),
            ("search_min_score", p.search_min_score),
            ("search_relative_cut", p.search_relative_cut),
            ("rerank_trust_weight", p.rerank_trust_weight),
            (
                "claim_contradiction_min_confidence",
                p.claim_contradiction_min_confidence,
            ),
            ("delivery_floor", p.delivery_floor),
            ("weak_evidence_top", p.weak_evidence_top),
            ("weak_line_quantile", p.weak_line_quantile),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return fail(format!("policy.{name} {value} out of 0..=1"));
            }
        }
        if let Some(g) = p.conflict_nli_gate
            && !(0.0..=1.0).contains(&g)
        {
            return fail(format!("policy.conflict_nli_gate {g} out of 0..=1"));
        }
        if let Some(g) = p.conflict_bridge_gate
            && !(0.0..=1.0).contains(&g)
        {
            return fail(format!("policy.conflict_bridge_gate {g} out of 0..=1"));
        }
        if let Some(c) = p.knee_cliff {
            if !(0.0..=1.0).contains(&c) {
                return fail(format!("policy.knee_cliff {c} out of 0..=1"));
            }
            // 0 is not "off": every relative drop is >= 0, so a zero cliff
            // trims EVERY score curve at its largest drop — the harshest
            // setting, not the absence of one. Off is null. (Found by the
            // ForgetEval adapter, which zeroed it to disable it.)
            if c <= 0.0 {
                return fail(
                    "policy.knee_cliff 0 would trim every result at its largest score drop; \
                     set it to null to turn the knee trim off"
                        .into(),
                );
            }
        }
        if !(4..=256).contains(&p.weak_line_probes) {
            return fail(format!(
                "policy.weak_line_probes {} out of 4..=256",
                p.weak_line_probes
            ));
        }
        // A rank demotion, not a 0..1 weight: measured in list positions. The
        // candidate pool is at most 50 deep, so anything past that is a hard
        // one-per-session cap wearing a bigger number.
        if !(0.0..=50.0).contains(&p.session_diversity_demote) {
            return fail(format!(
                "policy.session_diversity_demote {} out of 0..=50",
                p.session_diversity_demote
            ));
        }
        // A rank-fusion constant, not a 0..1 weight: it is added to a 1-based
        // rank, so anything non-positive divides by zero at rank 0 or inverts
        // the ordering outright.
        if let Some(t) = p.twin_trust_order
            && !(0.0..=1.0).contains(&t)
        {
            return fail(format!("policy.twin_trust_order {t} out of 0..=1"));
        }
        if let Some(k) = p.rerank_vote_k
            && !(k.is_finite() && k > 0.0)
        {
            return fail(format!("policy.rerank_vote_k {k} must be positive"));
        }
        if p.conflict_suspect_similarity > p.duplicate_similarity {
            return fail(
                "conflict_suspect_similarity above duplicate_similarity leaves an unreachable suspect band".into(),
            );
        }

        let b = &self.brief;
        if !(1_000..=200_000).contains(&b.total_chars) {
            return fail(format!(
                "brief.total_chars {} out of 1000..=200000",
                b.total_chars
            ));
        }
        if b.home_reserve > b.total_chars {
            return fail("brief.home_reserve exceeds the total budget".into());
        }
        for (name, t) in [("tags", &b.tags), ("suspects", &b.suspects)] {
            if t.cap > 100 {
                return fail(format!("brief.{name}.cap {} out of 0..=100", t.cap));
            }
        }
        validate_section(&b.recent, "brief.recent")?;
        validate_section(&b.open, "brief.open")?;
        validate_section(&b.handoff, "brief.handoff")?;
        validate_section(&b.cycle, "brief.cycle")?;
        if !matches!(b.canon_order.as_str(), "endorsed" | "connected") {
            return fail(format!(
                "brief.canon_order {:?} is not one of \"endorsed\", \"connected\"",
                b.canon_order
            ));
        }

        for p in &self.history.exclude_paths {
            if p.trim().is_empty() {
                return fail("history.exclude_paths contains an empty entry".into());
            }
        }
        Ok(())
    }
}

fn validate_section(s: &BriefSection, what: &str) -> crate::Result<()> {
    if s.cap > 100 {
        return Err(crate::Error::Config(format!(
            "{what}: cap {} out of 0..=100",
            s.cap
        )));
    }
    if !(20..=2_000).contains(&s.excerpt) {
        return Err(crate::Error::Config(format!(
            "{what}: excerpt {} out of 20..=2000",
            s.excerpt
        )));
    }
    Ok(())
}
