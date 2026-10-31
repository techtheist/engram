//! Crowding (0.11.0): how many notes sit unusually close to each note in
//! embedding space — the measure that "sleep" (consolidation after the fact)
//! starts from. Computed from the node-level vectors the store already keeps;
//! no model runs here, and nothing here ranks retrieval.
//!
//! The "close" band is relative to the graph itself: the similarity that only
//! the top [`BAND_TOP`] of this graph's pairs reach. Absolute cosine levels
//! don't carry between graphs (a prose-register graph sits higher everywhere
//! — Insight 00b22705tdob), but "closer than almost every other pair here"
//! does. [`BAND_FLOOR`] keeps a graph of strangers from calling its least
//! distant strangers crowded.
//!
//! Subjects are what the sleep reviews: notes joined by chains of close
//! pairs, cut adaptively. At the band itself a graph's subjects touch and
//! chain into one blob (409 of 562 notes on this repo's graph), so a group
//! larger than a subject may be is split again at a stricter cosine, and
//! again, until its pieces fit — a single-link hierarchy cut per branch, so
//! a tight subject keeps the members a looser one would lose. Groups below
//! [`Subjects::min`] (2% of the notes) are pockets, not subjects.

use std::collections::HashMap;

/// The share of all pairs that counts as close (top 1%).
pub const BAND_TOP: f64 = 0.01;
/// The band never drops below this cosine, whatever the percentile says.
pub const BAND_FLOOR: f32 = 0.55;
/// Neighbours kept per node; crowding counts within these, so it saturates
/// here (a note with this many close neighbours is crowded either way).
pub const NEAREST: usize = 24;
/// A subject holds at least this share of the indexed notes…
pub const SUBJECT_SHARE: f64 = 0.02;
/// …and never fewer than this many, however small the graph.
pub const SUBJECT_FLOOR: usize = 5;
/// A subject may hold this many times its minimum before it is split.
pub const SUBJECT_SPAN: usize = 4;
/// How much stricter each split's cosine gets.
const SPLIT_STEP: f32 = 0.005;

/// One note's place in the index.
#[derive(Debug, Clone, Default)]
pub struct Crowd {
    /// Neighbours inside the band (capped at [`NEAREST`]).
    pub crowding: usize,
    /// crowding scaled to 0..1 against this graph's crowded end (its 95th
    /// percentile), for colouring.
    pub heat: f64,
    /// The nearest notes, closest first: (id, cosine).
    pub nearest: Vec<(String, f32)>,
}

/// A subject: notes joined by chains of close pairs.
#[derive(Debug, Clone)]
pub struct Subject {
    /// Most central first: by summed similarity to the other members.
    pub members: Vec<String>,
    /// The cosine at which this group holds together (≥ the band; higher
    /// when it had to be split out of a larger one).
    pub cohesion: f32,
    /// Mean cosine over the close pairs inside it.
    pub density: f32,
}

/// The size bounds subjects were cut to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subjects {
    pub min: usize,
    pub max: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CrowdIndex {
    pub band: f32,
    /// Notes the index was built over.
    pub size: usize,
    pub nodes: HashMap<String, Crowd>,
    /// What it was built from — the cache key (see `Engine::crowding`).
    pub fingerprint: u64,
}

fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// Eight running sums instead of one: a single float sum must add in order
/// and never vectorizes; eight independent lanes do.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let ((ac, ar), (bc, br)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let tail: f32 = ar.iter().zip(br).map(|(x, y)| x * y).sum();
    for (x, y) in ac.iter().zip(bc) {
        for (lane, (p, q)) in lanes.iter_mut().zip(x.iter().zip(y)) {
            *lane += p * q;
        }
    }
    lanes.iter().sum::<f32>() + tail
}

/// Similarity histogram resolution over [-1, 1].
const BINS: usize = 4000;

/// Floor, not round: a bin's value is its lower edge, so the band never
/// lands a hair above the pairs that set it.
fn bin(s: f32) -> usize {
    (((s.clamp(-1.0, 1.0) + 1.0) / 2.0) * (BINS - 1) as f32).floor() as usize
}

impl CrowdIndex {
    /// Build over (id, vector) pairs. Brute force: n²/2 dot products, about
    /// a second at 5k notes in a release build — cached by the caller.
    pub fn build(items: &[(String, Vec<f32>)], fingerprint: u64) -> Self {
        let n = items.len();
        let vecs: Vec<Vec<f32>> = items.iter().map(|(_, v)| normalize(v)).collect();
        let mut hist = vec![0u64; BINS];
        let mut near: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
        // Each list's weakest kept similarity: a full list is only scanned
        // when a pair beats it, which most pairs don't.
        let mut worst = vec![f32::NEG_INFINITY; n];
        let keep = |list: &mut Vec<(usize, f32)>, worst: &mut f32, j: usize, s: f32| {
            if list.len() < NEAREST {
                list.push((j, s));
            } else if s > *worst
                && let Some(at) = list
                    .iter()
                    .enumerate()
                    .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
                    .map(|(at, _)| at)
            {
                list[at] = (j, s);
            } else {
                return;
            }
            if list.len() == NEAREST {
                *worst = list.iter().map(|e| e.1).fold(f32::INFINITY, f32::min);
            }
        };
        for i in 0..n {
            for j in (i + 1)..n {
                if vecs[i].len() != vecs[j].len() {
                    continue;
                }
                let s = dot(&vecs[i], &vecs[j]);
                hist[bin(s)] += 1;
                if s > worst[i] {
                    keep(&mut near[i], &mut worst[i], j, s);
                }
                if s > worst[j] {
                    keep(&mut near[j], &mut worst[j], i, s);
                }
            }
        }
        let pairs: u64 = hist.iter().sum();
        let band = if pairs == 0 {
            1.0
        } else {
            // Walk down from the top until BAND_TOP of the pairs are above.
            let want = ((pairs as f64) * BAND_TOP).ceil().max(1.0) as u64;
            let mut seen = 0u64;
            let mut at = BINS - 1;
            for b in (0..BINS).rev() {
                seen += hist[b];
                at = b;
                if seen >= want {
                    break;
                }
            }
            (at as f32 / (BINS - 1) as f32) * 2.0 - 1.0
        }
        .max(BAND_FLOOR);
        let mut nodes = HashMap::with_capacity(n);
        let mut counts = Vec::with_capacity(n);
        for (i, list) in near.iter_mut().enumerate() {
            list.sort_by(|a, b| b.1.total_cmp(&a.1));
            let crowding = list.iter().filter(|(_, s)| *s >= band).count();
            counts.push(crowding);
            nodes.insert(
                items[i].0.clone(),
                Crowd {
                    crowding,
                    heat: 0.0,
                    nearest: list
                        .iter()
                        .map(|(j, s)| (items[*j].0.clone(), *s))
                        .collect(),
                },
            );
        }
        // Heat: against the graph's own crowded end, so the hottest few
        // notes read hot on a calm graph and a dense one alike.
        counts.sort_unstable();
        let top = counts
            .get(((n as f64) * 0.95) as usize)
            .copied()
            .unwrap_or(0)
            .max(1) as f64;
        for crowd in nodes.values_mut() {
            crowd.heat = (crowd.crowding as f64 / top).min(1.0);
        }
        Self {
            band,
            size: n,
            nodes,
            fingerprint,
        }
    }

    /// The size bounds for this index: [`SUBJECT_SHARE`] of its notes
    /// (at least [`SUBJECT_FLOOR`]) up to [`SUBJECT_SPAN`] times that.
    pub fn subject_bounds(&self) -> Subjects {
        let min = ((self.size as f64 * SUBJECT_SHARE).ceil() as usize).max(SUBJECT_FLOOR);
        Subjects {
            min,
            max: min * SUBJECT_SPAN,
        }
    }

    /// The subjects among the notes `admit` lets in, largest first. Each
    /// note sits in at most one; notes only a looser cut would join (the
    /// bridges between subjects) sit in none.
    pub fn subjects(&self, admit: impl Fn(&str) -> bool) -> Vec<Subject> {
        let bounds = self.subject_bounds();
        let mut ids: Vec<&String> = self.nodes.keys().filter(|id| admit(id)).collect();
        ids.sort();
        let at: HashMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        // Close pairs, each once (a < b), from either end's nearest list: a
        // pair can make one note's list and miss the other's when that one
        // has closer neighbours. The lists cap what a stricter cut can see,
        // which only ever drops weaker pairs.
        let mut pairs: Vec<(usize, usize, f32)> = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            for (other, s) in &self.nodes[*id].nearest {
                if *s < self.band {
                    break;
                }
                if let Some(&j) = at.get(other.as_str())
                    && i != j
                {
                    pairs.push((i.min(j), i.max(j), *s));
                }
            }
        }
        pairs.sort_by_key(|p| (p.0, p.1));
        pairs.dedup_by_key(|p| (p.0, p.1));

        let mut out = Vec::new();
        let mut todo: Vec<(Vec<usize>, f32)> =
            groups(&(0..ids.len()).collect::<Vec<_>>(), &pairs, self.band)
                .into_iter()
                .map(|g| (g, self.band))
                .collect();
        while let Some((group, cut)) = todo.pop() {
            if group.len() < bounds.min {
                continue;
            }
            let next = cut + SPLIT_STEP;
            if group.len() > bounds.max && next <= 1.0 {
                let inside: Vec<(usize, usize, f32)> = {
                    let set: std::collections::HashSet<usize> = group.iter().copied().collect();
                    pairs
                        .iter()
                        .filter(|(a, b, s)| *s >= next && set.contains(a) && set.contains(b))
                        .copied()
                        .collect()
                };
                todo.extend(groups(&group, &inside, next).into_iter().map(|g| (g, next)));
                continue;
            }
            // Centrality and density over the close pairs inside.
            let set: std::collections::HashSet<usize> = group.iter().copied().collect();
            let mut weight: HashMap<usize, f32> = HashMap::new();
            let (mut sum, mut count) = (0.0f32, 0usize);
            for (a, b, s) in pairs
                .iter()
                .filter(|(a, b, _)| set.contains(a) && set.contains(b))
            {
                *weight.entry(*a).or_default() += s;
                *weight.entry(*b).or_default() += s;
                sum += s;
                count += 1;
            }
            let mut members = group;
            members.sort_by(|a, b| {
                weight
                    .get(b)
                    .unwrap_or(&0.0)
                    .total_cmp(weight.get(a).unwrap_or(&0.0))
                    .then(a.cmp(b))
            });
            // Unsplittable (every pair at 1.0): keep the most central.
            members.truncate(bounds.max);
            out.push(Subject {
                members: members.into_iter().map(|i| ids[i].clone()).collect(),
                cohesion: cut,
                density: if count == 0 { 0.0 } else { sum / count as f32 },
            });
        }
        out.sort_by(|a, b| {
            b.members
                .len()
                .cmp(&a.members.len())
                .then(b.density.total_cmp(&a.density))
                .then(a.members[0].cmp(&b.members[0]))
        });
        out
    }
}

/// Connected groups of `members` under `pairs` at or above `cut`.
fn groups(members: &[usize], pairs: &[(usize, usize, f32)], cut: f32) -> Vec<Vec<usize>> {
    let mut parent: HashMap<usize, usize> = members.iter().map(|&m| (m, m)).collect();
    fn root(p: &mut HashMap<usize, usize>, mut x: usize) -> usize {
        while p[&x] != x {
            let up = p[&p[&x]];
            p.insert(x, up);
            x = up;
        }
        x
    }
    for &(a, b, s) in pairs {
        if s < cut || !parent.contains_key(&a) || !parent.contains_key(&b) {
            continue;
        }
        let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
        if ra != rb {
            parent.insert(ra.max(rb), ra.min(rb));
        }
    }
    let mut by: HashMap<usize, Vec<usize>> = HashMap::new();
    for &m in members {
        let r = root(&mut parent, m);
        by.entry(r).or_default().push(m);
    }
    let mut out: Vec<Vec<usize>> = by.into_values().collect();
    out.sort_by_key(|g| g[0]);
    out
}

/// Stable 64-bit FNV-1a over byte strings, each terminated — the same
/// construction the sweep memory uses, so marks survive a toolchain change.
pub fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &x in part.iter().chain(&[0u8]) {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 64;

    /// A deterministic pseudo-random unit-ish vector (LCG), for strangers.
    fn stranger(seed: u64) -> Vec<f32> {
        let mut x = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (0..DIM)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((x >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .collect()
    }

    /// `base` nudged along one axis — a paraphrase of the same note.
    fn near(base: &[f32], axis: usize, by: f32) -> Vec<f32> {
        let mut v = base.to_vec();
        v[axis] += by;
        v
    }

    /// Three notes on one subject among 60 strangers.
    fn sample() -> Vec<(String, Vec<f32>)> {
        let subject = stranger(9999);
        let mut items = vec![
            ("a".to_string(), subject.clone()),
            ("b".to_string(), near(&subject, 3, 0.2)),
            ("c".to_string(), near(&subject, 7, -0.2)),
        ];
        for i in 0..60 {
            items.push((format!("s{i}"), stranger(i)));
        }
        items
    }

    #[test]
    fn the_subject_pile_is_crowded_and_strangers_are_not() {
        let index = CrowdIndex::build(&sample(), 7);
        assert!(index.band >= BAND_FLOOR);
        for id in ["a", "b", "c"] {
            assert_eq!(index.nodes[id].crowding, 2, "{id}");
            assert_eq!(index.nodes[id].heat, 1.0);
        }
        for i in 0..60 {
            assert_eq!(index.nodes[&format!("s{i}")].crowding, 0, "s{i}");
        }
        assert_eq!(index.fingerprint, 7);
        assert_eq!(index.size, 63);
    }

    #[test]
    fn nearest_is_closest_first() {
        let index = CrowdIndex::build(&sample(), 0);
        let near = &index.nodes["a"].nearest;
        assert_eq!(near.len(), NEAREST);
        assert!(near.windows(2).all(|w| w[0].1 >= w[1].1));
        assert!(["b", "c"].contains(&near[0].0.as_str()));
        assert!(["b", "c"].contains(&near[1].0.as_str()));
    }

    /// `count` paraphrases of one subject, each nudged along its own axis.
    fn pile(prefix: &str, seed: u64, count: usize, by: f32) -> Vec<(String, Vec<f32>)> {
        let base = stranger(seed);
        (0..count)
            .map(|i| (format!("{prefix}{i}"), near(&base, i % DIM, by)))
            .collect()
    }

    fn strangers(count: usize) -> Vec<(String, Vec<f32>)> {
        (0..count)
            .map(|i| (format!("s{i}"), stranger(i as u64)))
            .collect()
    }

    #[test]
    fn subject_bounds_scale_with_the_graph() {
        let small = CrowdIndex::build(&strangers(40), 0);
        assert_eq!(
            small.subject_bounds(),
            Subjects {
                min: SUBJECT_FLOOR,
                max: SUBJECT_FLOOR * SUBJECT_SPAN
            }
        );
        let big = CrowdIndex {
            size: 1000,
            ..Default::default()
        };
        assert_eq!(big.subject_bounds().min, 20, "2% of the notes");
        assert_eq!(big.subject_bounds().max, 80);
    }

    #[test]
    fn a_pile_is_a_subject_and_a_pocket_is_not() {
        let mut items = strangers(200);
        items.extend(pile("p", 4242, 9, 0.1));
        items.extend(pile("q", 777, 3, 0.1)); // under the 5-note floor
        let index = CrowdIndex::build(&items, 0);
        let subjects = index.subjects(|_| true);
        assert_eq!(subjects.len(), 1, "{subjects:?}");
        assert_eq!(subjects[0].members.len(), 9);
        assert!(subjects[0].members.iter().all(|m| m.starts_with('p')));
        assert_eq!(subjects[0].cohesion, index.band, "fits at the band itself");
        assert!(subjects[0].density > 0.9);
    }

    #[test]
    fn an_oversized_group_is_split_into_its_tight_subjects() {
        // Two tight piles of 12 whose centres are themselves close: at the
        // band they chain into one group of 24, over the 20-note cap at
        // 224 notes; a stricter cut separates them.
        let mut items = strangers(200);
        let a = stranger(9001); // seeds past the strangers' own
        let lean = stranger(9002);
        let b: Vec<f32> = a.iter().zip(&lean).map(|(x, y)| x + 0.6 * y).collect();
        for i in 0..12 {
            items.push((format!("a{i}"), near(&a, i, 0.05)));
            items.push((format!("b{i}"), near(&b, i, 0.05)));
        }
        let index = CrowdIndex::build(&items, 0);
        assert_eq!(index.subject_bounds(), Subjects { min: 5, max: 20 });
        let subjects = index.subjects(|_| true);
        assert_eq!(subjects.len(), 2, "{subjects:?}");
        for s in &subjects {
            assert_eq!(s.members.len(), 12);
            let side = &s.members[0][..1];
            assert!(s.members.iter().all(|m| m.starts_with(side)), "{s:?}");
            assert!(s.cohesion > index.band, "cut tighter than the band");
        }
    }

    #[test]
    fn subjects_respect_the_admit_filter() {
        let mut items = strangers(200);
        items.extend(pile("p", 4242, 9, 0.1));
        let index = CrowdIndex::build(&items, 0);
        assert!(
            index
                .subjects(|id| id != "p0" && id != "p1" && id != "p2" && id != "p3" && id != "p4")
                .is_empty()
        );
        assert_eq!(index.subjects(|id| id != "p0")[0].members.len(), 8);
    }

    #[test]
    fn a_graph_of_strangers_has_no_crowding() {
        let items: Vec<_> = (0..40).map(|i| (format!("n{i}"), stranger(i))).collect();
        let index = CrowdIndex::build(&items, 0);
        assert_eq!(index.band, BAND_FLOOR, "the floor holds");
        assert!(
            index
                .nodes
                .values()
                .all(|c| c.crowding == 0 && c.heat == 0.0)
        );
        assert!(index.subjects(|_| true).is_empty());
    }

    #[test]
    fn tiny_graphs_build() {
        assert_eq!(CrowdIndex::build(&[], 0).size, 0);
        let one = CrowdIndex::build(&[("x".into(), stranger(1))], 0);
        assert_eq!(one.nodes["x"].crowding, 0);
        assert_eq!(one.band, 1.0);
    }

    #[test]
    fn fnv_is_frozen() {
        // Reviewed marks persist across releases keyed on this value.
        assert_eq!(fnv(&[b"engram", b"sleep"]), FROZEN);
    }

    const FROZEN: u64 = 15_070_237_596_367_586_774;
}
