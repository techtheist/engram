//! Per-graph fitting of the conflict floor and the weak line from synthetic probes.

use super::*;

impl Engine {
    /// Dial one: fit the conflict-suspect floor from judged history (max balanced
    /// accuracy over midpoints, higher on ties for a quieter queue). Damped and
    /// clamped into [`crate::policy::AUTO_TUNE_FLOOR_MIN`, `duplicate_similarity`). Mutates
    /// `cfg`, returns the journal fragment.
    pub(super) fn fit_conflict_floor(
        &self,
        cfg: &mut crate::config::GraphConfig,
    ) -> Result<Option<String>> {
        use crate::policy::{
            AUTO_TUNE_FLOOR_MIN, AUTO_TUNE_MIN_DELTA, AUTO_TUNE_MIN_EACH, AUTO_TUNE_MIN_JUDGED,
        };
        let judged: Vec<(f64, bool)> = self
            .store
            .all_suspects()?
            .into_iter()
            // Only pairs the SIMILARITY path could have raised inform the
            // similarity dial: the NLI path nominates below the
            // floor, and letting those verdicts into the fit would drag the
            // floor toward the NLI band it never reads.
            .filter(|s| s.similarity >= AUTO_TUNE_FLOOR_MIN)
            .filter_map(|s| match s.status {
                SuspectStatus::Confirmed => Some((s.similarity, true)),
                SuspectStatus::Dismissed => Some((s.similarity, false)),
                SuspectStatus::Suspected => None,
            })
            .collect();
        let confirmed = judged.iter().filter(|(_, c)| *c).count();
        let dismissed = judged.len() - confirmed;
        if judged.len() < AUTO_TUNE_MIN_JUDGED
            || confirmed < AUTO_TUNE_MIN_EACH
            || dismissed < AUTO_TUNE_MIN_EACH
        {
            return Ok(None);
        }

        let mut sims: Vec<f64> = judged.iter().map(|(s, _)| *s).collect();
        sims.sort_by(f64::total_cmp);
        sims.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        let old = cfg.policy.conflict_suspect_similarity;
        let mut best = (old, f64::NEG_INFINITY);
        for w in sims.windows(2) {
            let t = (w[0] + w[1]) / 2.0;
            let kept = judged.iter().filter(|(s, c)| *c && *s >= t).count() as f64;
            let silenced = judged.iter().filter(|(s, c)| !*c && *s < t).count() as f64;
            let balanced = (kept / confirmed as f64 + silenced / dismissed as f64) / 2.0;
            if balanced > best.1 + 1e-9 || (balanced > best.1 - 1e-9 && t > best.0) {
                best = (t, balanced);
            }
        }
        let fitted = best.0;
        // Damped, then hard-clamped: the dial travels half the distance to
        // the fit per pass (policy::AUTO_TUNE_DAMPING), and the clamp reads
        // the DAMPED value — a glitched fit, or a corrupt stored dial, lands
        // back inside the band on the very next pass.
        let (lo, hi) = (AUTO_TUNE_FLOOR_MIN, cfg.policy.duplicate_similarity - 0.001);
        let anchor = if old.is_finite() {
            old.clamp(lo, hi)
        } else {
            hi
        };
        let target = if fitted.is_finite() {
            (anchor + (fitted - anchor) * crate::policy::AUTO_TUNE_DAMPING).clamp(lo, hi)
        } else {
            anchor
        };
        if (target - old).abs() < AUTO_TUNE_MIN_DELTA {
            return Ok(None);
        }

        cfg.policy.conflict_suspect_similarity = target;
        Ok(Some(format!(
            "conflict floor {old:.3} -> {target:.3} (fit {fitted:.3}, damped) from {} judged pairs ({confirmed} confirmed / {dismissed} dismissed)",
            judged.len()
        )))
    }

    /// Dial two: fit the weak line as `weak_line_quantile` of the top scores
    /// phantom probes reach here ("how high can a score climb when the answer
    /// doesn't exist"). Probes never stamp `last_seen`. Damped and clamped up to
    /// [`crate::policy::WEAK_LINE_MAX`]. Mutates `cfg`, returns the journal fragment.
    pub(super) fn fit_weak_line(
        &self,
        cfg: &mut crate::config::GraphConfig,
    ) -> Result<Option<String>> {
        use crate::policy::{AUTO_TUNE_MIN_DELTA, WEAK_LINE_ABOVE_FLOOR, WEAK_LINE_MAX};
        let Some(reranker) = &self.reranker else {
            return Ok(None);
        };
        // Probes borrow the graph's own title vocabulary: generic wording under-reads
        // how high this graph's scores climb. The coined subject keeps them unanswerable.
        let mut notes = self.store.all_nodes()?;
        notes.retain(|n| n.valid_until.is_none());
        // Content order, not id order: ids come from the clock, so the same corpus
        // would sample differently every run (eval Finding 00dvpn0t6gx6).
        notes.sort_by_cached_key(|n| (content_hash(&n.title, n.body.as_deref()), n.id.clone()));
        let vocab: Vec<String> = notes.iter().filter_map(|n| probe_terms(&n.title)).collect();
        // Two probe families (templates over borrowed words, and ICT transplants of
        // real sentences); the fit takes the louder family's quantile.
        let total = cfg.policy.weak_line_probes;
        let top_of = |probe: &str, source: Option<&str>| -> Result<f64> {
            let qv = self.embedder.embed_one(probe)?;
            // Probes calibrate against the WHOLE graph — a windowed noise
            // sample would fit a line to a slice of history.
            let mut hits = self.store.search_hybrid(
                probe,
                Some(&qv),
                &[],
                12,
                Default::default(),
                &Default::default(),
            )?;
            // A transplant is asked without its source note (Problem 00e353wiopga): a long
            // real sentence still points at its source and pinned the line at 0.95.
            if let Some(source) = source {
                hits.retain(|h| h.id != source);
            }
            if hits.is_empty() {
                return Ok(0.0);
            }
            self.rerank(reranker.as_ref(), probe, &mut hits);
            Ok(hits.iter().map(|h| h.score).fold(f64::MIN, f64::max))
        };
        let mut template_tops = Vec::with_capacity(total.div_ceil(2));
        let mut transplant_tops = Vec::with_capacity(total / 2);
        for i in 0..total {
            if i % 2 == 0 {
                let terms = (!vocab.is_empty()).then(|| vocab[(i / 2) % vocab.len()].as_str());
                template_tops.push(top_of(&phantom_probe(i / 2, terms), None)?);
            } else {
                let n = &notes[(i / 2) * 17 % notes.len()];
                transplant_tops.push(top_of(
                    &transplant_probe(i / 2, &n.title, n.body.as_deref()),
                    Some(&n.id),
                )?);
            }
        }
        template_tops.sort_by(f64::total_cmp);
        transplant_tops.sort_by(f64::total_cmp);
        let q = cfg.policy.weak_line_quantile;
        let fitted = quantile(&template_tops, q).max(quantile(&transplant_tops, q));
        if std::env::var_os("ENGRAM_WEAK_LINE_DEBUG").is_some() {
            eprintln!(
                "weak line fit: template q{:.0} {:.3} {:?}\n               transplant q{:.0} {:.3} {:?}",
                q * 100.0,
                quantile(&template_tops, q),
                template_tops
                    .iter()
                    .map(|x| (x * 100.0).round() / 100.0)
                    .collect::<Vec<_>>(),
                q * 100.0,
                quantile(&transplant_tops, q),
                transplant_tops
                    .iter()
                    .map(|x| (x * 100.0).round() / 100.0)
                    .collect::<Vec<_>>(),
            );
        }
        let old = cfg.policy.weak_evidence_top;
        // Damped, then clamped. The lower clamp is floor-relative because the score
        // scale is per-graph (policy::WEAK_LINE_ABOVE_FLOOR).
        let lo = (cfg.policy.delivery_floor + WEAK_LINE_ABOVE_FLOOR).clamp(0.0, WEAK_LINE_MAX);
        let anchor = if old.is_finite() {
            old.clamp(lo, WEAK_LINE_MAX)
        } else {
            WEAK_LINE_MAX
        };
        let target = if fitted.is_finite() {
            (anchor + (fitted - anchor) * crate::policy::AUTO_TUNE_DAMPING).clamp(lo, WEAK_LINE_MAX)
        } else {
            anchor
        };
        if (target - old).abs() < AUTO_TUNE_MIN_DELTA {
            return Ok(None);
        }
        cfg.policy.weak_evidence_top = target;
        Ok(Some(format!(
            "weak line {old:.3} -> {target:.3} (fit {fitted:.3}, damped) from {} phantom probes at q{:.0}, two families",
            template_tops.len() + transplant_tops.len(),
            q * 100.0
        )))
    }
}

/// The knee of a descending score curve: the largest relative drop, if at
/// least `min_cliff` of the running score ([`crate::policy::KNEE_MIN_CLIFF`]).
/// Returns the minimum score to keep, or `None` without a cliff.
pub(super) fn knee_floor(sorted_desc: &[f64], min_cliff: f64) -> Option<f64> {
    if sorted_desc.len() <= 1 {
        return None;
    }
    let (mut best_at, mut best_drop) = (0usize, 0.0f64);
    for i in 1..sorted_desc.len() {
        let prev = sorted_desc[i - 1].max(1e-9);
        let drop = (sorted_desc[i - 1] - sorted_desc[i]) / prev;
        if drop > best_drop {
            best_drop = drop;
            best_at = i;
        }
    }
    (best_drop >= min_cliff).then(|| sorted_desc[best_at - 1])
}

/// Higher quantile of an ASC-sorted sample (the split-conformal convention:
/// round up, never interpolate — the threshold must be a score that was
/// actually reached).
pub(super) fn quantile(sorted_asc: &[f64], q: f64) -> f64 {
    if sorted_asc.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_asc.len() - 1) as f64 * q).ceil() as usize;
    sorted_asc[idx.min(sorted_asc.len() - 1)]
}

/// Up to two long content words from a note title — the register the
/// weak-line probes borrow so they ask the way this graph is written.
fn probe_terms(title: &str) -> Option<String> {
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric() && !"-_.".contains(c))
        .filter(|w| w.len() >= 5)
        .take(2)
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// The i-th deterministic coinage: pronounceable, unmistakably not a word
/// this graph contains. Deterministic — the same graph calibrates against
/// the same probes, so the fitted line settles instead of wandering.
fn coined(i: usize) -> String {
    const ONSET: [&str; 8] = ["vor", "zel", "quam", "dro", "fex", "gril", "plom", "stru"];
    const MID: [&str; 8] = ["ni", "ba", "ro", "ka", "lu", "tri", "gos", "pem"];
    const CODA: [&str; 8] = ["dax", "vek", "morn", "lisk", "tor", "bran", "funt", "gel"];
    format!("{}{}{}", ONSET[i % 8], MID[(i / 8) % 8], CODA[(i / 64) % 8])
}

/// Mint the i-th template phantom probe for the weak-line fit: a
/// memory-shaped question about a coined subject that cannot exist in any
/// graph, phrased over vocabulary borrowed from the graph itself when it
/// has any.
fn phantom_probe(i: usize, terms: Option<&str>) -> String {
    const FALLBACK: [&str; 6] = [
        "subsystem",
        "migration",
        "deployment",
        "cache layer",
        "retry worker",
        "pipeline",
    ];
    const TEMPLATES: [&str; 6] = [
        "What did we decide about the {}?",
        "Why does the {} keep failing?",
        "What broke the last time the {} was deployed?",
        "Which constraint governs the {}?",
        "What is the policy for the {}?",
        "What did the {} replace and why?",
    ];
    let topic = format!(
        "{} {}",
        coined(i),
        terms.unwrap_or(FALLBACK[i % FALLBACK.len()])
    );
    TEMPLATES[i % TEMPLATES.len()].replace("{}", &topic)
}

/// The i-th transplant probe: a real sentence with its two most distinctive
/// words swapped for coinages (inverted ICT, ACL P19-1612). It reads like the
/// oblique prose queries that score highest on absent subjects, the ceiling
/// the line must clear (template q90 0.48 vs real 0.77 at 1500 notes).
fn transplant_probe(i: usize, title: &str, body: Option<&str>) -> String {
    // The longest sentence in the body carries the most register; a short
    // or empty body falls back to the title.
    let sentence = body
        .unwrap_or("")
        .split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|s| s.len() >= 40)
        .max_by_key(|s| s.len().min(220))
        .unwrap_or(title);
    let sentence: String = sentence.chars().take(220).collect();

    // Swap the two longest content words — the subject-bearing tokens in
    // almost any technical sentence — for coinages.
    let mut words: Vec<&str> = sentence
        .split(|c: char| !c.is_alphanumeric() && !"-_".contains(c))
        .filter(|w| w.len() >= 6)
        .collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    words.dedup();
    let mut out = sentence.clone();
    for (n, w) in words.into_iter().take(2).enumerate() {
        out = out.replacen(w, &coined(i + 37 * (n + 1)), 1);
    }
    out
}
