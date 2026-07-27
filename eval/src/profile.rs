//! The shape of a real memory node, extracted from this repo's own graph.
//!
//! The first version of this corpus wrote 150-character bodies into a graph
//! with no edges. Real nodes have ~750-character bodies and a median degree of
//! 2, with only 4% isolated. Those are not cosmetic differences: body length is
//! how much surface an oblique question has to match against, and edges are the
//! entire mechanism by which a graph memory beats a flat one. Measuring
//! retrieval on short, disconnected documents measures something the product
//! never does.
//!
//! So the generator fills to a *profile* rather than to a constant, and the
//! profile is a variable — the defaults below are measurements, and any of them
//! can be moved to ask "what if our notes were terser?".
//!
//! Measured 2026-07-26 over 30 randomly sampled nodes (seed 7) from a 279-node
//! graph, cross-checked against all 279.

use serde::Serialize;

/// Quartiles of an observed distribution. Sampling reproduces the shape
/// instead of collapsing it to a mean, because the tails are where the
/// interesting documents live — a 2000-character node behaves very
/// differently from a 200-character one.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Quartiles {
    pub min: usize,
    pub p25: usize,
    pub median: usize,
    pub p75: usize,
    pub max: usize,
}

impl Quartiles {
    /// Draw a value reproducing the quartile shape: a quarter of draws land in
    /// each band, uniformly within it.
    pub fn sample(&self, r: u64) -> usize {
        let band = r % 4;
        let (lo, hi) = match band {
            0 => (self.min, self.p25),
            1 => (self.p25, self.median),
            2 => (self.median, self.p75),
            _ => (self.p75, self.max),
        };
        let span = hi.saturating_sub(lo).max(1);
        lo + ((r >> 8) as usize % span)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub title_chars: Quartiles,
    pub body_chars: Quartiles,
    /// Mean edges per node in the real graph (1.06 — a sparse, tree-ish graph,
    /// not a dense mesh).
    pub edges_per_node: f64,
    /// Share of nodes with no edges at all.
    pub isolated: f64,
    /// Verb distribution, in the order the generator prefers them.
    pub verb_mix: &'static [(&'static str, f64)],
    pub code_refs: Quartiles,
}

impl Default for Profile {
    /// This repo's graph, 2026-07-26.
    fn default() -> Self {
        Self {
            title_chars: Quartiles {
                min: 28,
                p25: 73,
                median: 89,
                p75: 122,
                max: 191,
            },
            body_chars: Quartiles {
                min: 133,
                p25: 461,
                median: 748,
                p75: 1209,
                max: 2250,
            },
            edges_per_node: 1.06,
            isolated: 0.04,
            verb_mix: &[
                ("about", 0.328),
                ("builds-on", 0.243),
                ("because", 0.226),
                ("answers", 0.155),
                ("replaces", 0.024),
                ("needs", 0.020),
                ("conflicts-with", 0.003),
            ],
            code_refs: Quartiles {
                min: 0,
                p25: 0,
                median: 1,
                p75: 2,
                max: 4,
            },
        }
    }
}

impl Profile {
    /// A corpus of short, disconnected notes — what the first version of this
    /// harness measured, kept so the difference can be quantified rather than
    /// asserted.
    pub fn terse() -> Self {
        Self {
            body_chars: Quartiles {
                min: 120,
                p25: 140,
                median: 150,
                p75: 165,
                max: 190,
            },
            edges_per_node: 0.0,
            isolated: 1.0,
            ..Self::default()
        }
    }

    /// Pick a verb by cumulative weight.
    pub fn verb(&self, r: u64) -> &'static str {
        let total: f64 = self.verb_mix.iter().map(|(_, w)| w).sum();
        let mut point = (r % 10_000) as f64 / 10_000.0 * total;
        for (verb, w) in self.verb_mix {
            if point < *w {
                return verb;
            }
            point -= w;
        }
        self.verb_mix.last().map(|(v, _)| *v).unwrap_or("about")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_reproduces_the_quartiles() {
        let q = Profile::default().body_chars;
        let draws: Vec<usize> = (0..4000)
            .map(|i| q.sample(i as u64 * 2_654_435_761))
            .collect();
        let below_median = draws.iter().filter(|d| **d <= q.median).count();
        let ratio = below_median as f64 / draws.len() as f64;
        assert!(
            (0.35..0.65).contains(&ratio),
            "half the draws should sit below the median, got {ratio}"
        );
        assert!(draws.iter().all(|d| *d >= q.min && *d <= q.max));
    }

    #[test]
    fn the_verb_mix_favours_what_the_real_graph_favours() {
        let p = Profile::default();
        let mut counts = std::collections::HashMap::new();
        for i in 0..10_000u64 {
            *counts
                .entry(p.verb(i.wrapping_mul(2_654_435_761)))
                .or_insert(0) += 1;
        }
        let about = counts.get("about").copied().unwrap_or(0);
        let conflicts = counts.get("conflicts-with").copied().unwrap_or(0);
        assert!(about > 2000, "about is a third of real edges, got {about}");
        assert!(
            conflicts < about / 10,
            "conflicts-with is rare in a real graph"
        );
    }

    #[test]
    fn the_terse_profile_is_the_old_corpus() {
        let t = Profile::terse();
        assert_eq!(t.isolated, 1.0);
        assert_eq!(t.edges_per_node, 0.0);
        assert!(t.body_chars.median < Profile::default().body_chars.median / 4);
    }
}
