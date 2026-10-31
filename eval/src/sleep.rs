//! The sleep bench (0.11.0): do the review queue's discrete mechanisms find
//! what was planted? A regression floor for crowding, not a retrieval score.
//!
//! THE DESIGN. A background of generated facts (each about its own invented
//! subject) plus six planted **piles**: notes all about one invented subject
//! — the fact restated several ways, and other facts' claims moved onto that
//! subject. A pile is what a graph grows when one subject keeps coming up,
//! and it is what a sleep exists to consolidate. The ground truth is exact:
//! which notes belong to which pile, which invented name a pile is about,
//! which pile is settled history and which is live work, and which notes
//! nothing reaches.
//!
//! Four mechanisms, four scores:
//!
//! * **A — subjects.** Is each pile found as one subject (`recovered`:
//!   some subject holds ≥80% of it at ≥80% purity)? How pure are the
//!   subjects that match a pile, and how many subjects are background only?
//! * **B — names.** Does the matched subject's name carry the pile's
//!   invented name ("Kelulvor")?
//! * **C — worth.** Piles come in three classes of two: settled history
//!   spread over five versions, settled in one version, and this cycle's
//!   live work. Every cross-class pair must rank in that order.
//! * **D — islands.** Every note carries a tag except a planted handful; the
//!   islands view must return exactly those.
//!
//! No model writes anything and no judge reads anything: with a fixed seed
//! and embedder, a run is byte-for-byte repeatable.

use std::collections::HashSet;

use engram_core::{Durability, Engine, NewNode, ReviewQuery, Source, SqliteStore, Store as _};
use serde::Serialize;

use crate::generate::{Fact, corpus_full};
use crate::run::{Config, embedder};

const DAY: i64 = 86_400;
/// Planted piles; two per worth class.
const PILES: usize = 6;
/// Untagged, unlinked notes planted among the background.
const ISLANDS: usize = 8;
/// A pile counts as found when one subject holds this share of it at this
/// purity.
const FOUND: f64 = 0.8;
/// The graph's working version while the bench runs.
const WORKING: &str = "1.0.0";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum PileClass {
    /// Backdated over months, stamped across five versions.
    SettledSpread,
    /// Backdated a month, all in one past version.
    SettledOne,
    /// Written now, in the working version.
    Live,
}

#[derive(Debug, Clone, Serialize)]
pub struct PileResult {
    pub name: String,
    pub class: PileClass,
    pub size: usize,
    /// Share of the pile inside its best-matching subject.
    pub coverage: f64,
    /// The same, for its restatements of one claim and for its other
    /// claims about the subject.
    pub coverage_restated: f64,
    pub coverage_aspects: f64,
    /// Share of that subject drawn from the pile.
    pub purity: f64,
    pub recovered: bool,
    /// The matched subject's name, if any.
    pub subject_name: Option<String>,
    pub named: bool,
    /// 1-based rank of the matched subject in the review queue.
    pub rank: Option<usize>,
    pub worth: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SizeReport {
    pub notes: usize,
    pub band: f32,
    pub min_size: usize,
    pub max_size: usize,
    pub subjects: usize,
    /// Subjects no pile holds the majority of.
    pub background_subjects: usize,
    pub piles: Vec<PileResult>,
    /// A — piles found / planted.
    pub recovered: f64,
    pub mean_purity: f64,
    /// B — matched subjects whose name carries the invented name.
    pub named: f64,
    /// C — cross-class pile pairs ranked in class order.
    pub worth_order: f64,
    /// D — planted islands returned / planted, and returned islands that
    /// were planted / returned.
    pub island_recall: f64,
    pub island_precision: f64,
    pub millis: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct SleepReport {
    pub embedder: String,
    pub embeddings_are_fake: bool,
    pub seed: u64,
    pub sizes: Vec<SizeReport>,
}

/// The invented word of a subject ("Kelulvor" of "Kelulvor sweep worker").
fn invented(subject: &str) -> &str {
    subject.split_whitespace().next().unwrap_or(subject)
}

fn node(f_title: String, body: String, f: &Fact, tags: Vec<String>) -> NewNode {
    NewNode {
        node_type: f.kind.node_type(),
        title: f_title,
        body: Some(body),
        created_at: None,
        durability: Durability::Stable,
        source: Source::Claude,
        session_id: None,
        status: f.kind.status(),
        code_refs: vec![],
        tags,
        version: None,
        props: None,
        fields: None,
    }
}

/// One pile of `size` notes about `core`'s subject: half the fact restated
/// (the same claim noted again across releases), half claims borrowed from
/// `donors` with their subject swapped for this one (other aspects of the
/// same subject). The flag marks a restatement.
fn pile(core: &Fact, donors: &mut impl Iterator<Item = Fact>, size: usize) -> Vec<(NewNode, bool)> {
    let s = &core.subject;
    let tag = vec![core.kind.node_type().as_str().to_lowercase()];
    let claim = core.title.replace(s.as_str(), "it");
    let wrappers = [
        core.title.clone(),
        format!("{s}: {claim}"),
        format!("Confirmed again: {}", core.title),
        format!("Status of the {s}: it still {}", core.predicate),
        format!("The {s} and {}", core.answer),
        format!("Still true of the {s}: {claim}"),
    ];
    let qualifiers = [
        "",
        " (noted again after the release)",
        " — re-checked during review",
        " — seen again in production",
        " — carried forward unchanged",
        " — raised once more in planning",
    ];
    let restated = size / 2;
    let mut out: Vec<(NewNode, bool)> = (0..restated)
        .map(|i| {
            let title = format!(
                "{}{}",
                wrappers[i % wrappers.len()],
                qualifiers[(i / wrappers.len()) % qualifiers.len()]
            );
            (node(title, core.body.clone(), core, tag.clone()), true)
        })
        .collect();
    while out.len() < size {
        let Some(d) = donors.next() else { break };
        if !d.title.contains(d.subject.as_str()) {
            continue;
        }
        out.push((
            node(
                d.title.replace(d.subject.as_str(), s),
                d.body.replace(d.subject.as_str(), s),
                &d,
                tag.clone(),
            ),
            false,
        ));
    }
    out
}

/// What a subject is beside its invented name ("sweep worker").
fn component(subject: &str) -> &str {
    subject.split_once(' ').map_or("", |(_, rest)| rest)
}

fn class_of(i: usize) -> PileClass {
    match i % 3 {
        0 => PileClass::SettledSpread,
        1 => PileClass::SettledOne,
        _ => PileClass::Live,
    }
}

/// Stamp a pile's dates and versions for its class.
fn date(nodes: &mut [NewNode], class: PileClass, now: i64) {
    let n = nodes.len().max(1);
    for (i, nn) in nodes.iter_mut().enumerate() {
        match class {
            PileClass::SettledSpread => {
                nn.created_at = Some(now - (200 - (i * 160 / n) as i64) * DAY);
                nn.version = Some(format!("0.{}.0", 1 + i * 5 / n));
            }
            PileClass::SettledOne => {
                nn.created_at = Some(now - (40 + (i % 5) as i64) * DAY);
                nn.version = Some("0.5.0".into());
            }
            PileClass::Live => {}
        }
    }
}

fn run_size(cfg: &Config, notes: usize) -> anyhow::Result<SizeReport> {
    let started = std::time::Instant::now();
    let min = (notes * 2 / 100).max(5);
    let sizes: Vec<usize> = (0..PILES).map(|i| min + min * (i / 2) / 2).collect();
    let planted: usize = sizes.iter().sum();
    let background = notes.saturating_sub(planted);
    // One corpus, split three ways: the background, the piles' core facts,
    // and donors whose claims are moved onto a pile's subject. Donors are
    // never written as themselves, so no background note shadows a pile.
    let total = background + PILES + planted * 2;
    let c = corpus_full(total, 0, cfg.seed, &cfg.profile, &cfg.type_mix);
    let mut facts = c.facts.into_iter();
    let back: Vec<Fact> = facts.by_ref().take(background).collect();
    // Each pile about a different component: piles about one component
    // chain into each other, which measures the generator, not the queue.
    let mut cores: Vec<Fact> = Vec::new();
    let mut rest: Vec<Fact> = Vec::new();
    for f in facts {
        if cores.len() < PILES
            && cores
                .iter()
                .all(|c| component(&c.subject) != component(&f.subject))
        {
            cores.push(f);
        } else {
            rest.push(f);
        }
    }
    let mut donors = rest.into_iter();

    let (emb, _) = embedder(cfg.embed_model.as_deref());
    let store = SqliteStore::open_in_memory()?;
    store.reset_vectors(emb.embed_one("dimension probe")?.len())?;
    let engine = Engine::new(store, emb);
    engine.set_current_version(Some(WORKING))?;
    let now = engram_core::now();

    let mut written = 0usize;
    let mut islands: HashSet<String> = HashSet::new();
    for (i, f) in back.iter().enumerate() {
        let island = i % (background / ISLANDS).max(1) == 0 && islands.len() < ISLANDS;
        let tags = if island {
            vec![]
        } else {
            vec![f.kind.node_type().as_str().to_lowercase()]
        };
        let n = engine.add_node(node(f.title.clone(), f.body.clone(), f, tags))?;
        written += 1;
        if island {
            islands.insert(n.id);
        }
    }
    let mut members: Vec<HashSet<String>> = Vec::new();
    let mut restatements: HashSet<String> = HashSet::new();
    for (i, core) in cores.iter().enumerate() {
        let (mut nodes, kinds): (Vec<NewNode>, Vec<bool>) =
            pile(core, &mut donors, sizes[i]).into_iter().unzip();
        date(&mut nodes, class_of(i), now);
        let mut ids = HashSet::new();
        for (nn, restated) in nodes.into_iter().zip(kinds) {
            let id = engine.add_node(nn)?.id;
            if restated {
                restatements.insert(id.clone());
            }
            ids.insert(id);
            written += 1;
        }
        members.push(ids);
    }

    // A, B, C: every subject in the queue, all members.
    let page = engine.review_page(&ReviewQuery {
        limit: usize::MAX,
        ..Default::default()
    })?;
    let subjects: Vec<(HashSet<String>, Option<String>, f64)> = page
        .subjects
        .iter()
        .map(|s| {
            let ids = s
                .members
                .iter()
                .map(|m| m.id.clone())
                .chain(s.more.iter().map(|m| m.id.clone()))
                .collect();
            (ids, s.name.clone(), s.worth.score)
        })
        .collect();
    let mut piles = Vec::new();
    for (i, core) in cores.iter().enumerate() {
        let pile = &members[i];
        let best = subjects
            .iter()
            .enumerate()
            .map(|(r, (ids, _, _))| (r, ids.intersection(pile).count(), ids.len()))
            .max_by_key(|&(r, hit, _)| (hit, std::cmp::Reverse(r)))
            .filter(|&(_, hit, _)| hit > 0);
        let (coverage, purity, rank) = match best {
            Some((r, hit, len)) => (
                hit as f64 / pile.len() as f64,
                hit as f64 / len as f64,
                Some(r),
            ),
            None => (0.0, 0.0, None),
        };
        let share = |restated: bool| {
            let part: Vec<&String> = pile
                .iter()
                .filter(|id| restatements.contains(*id) == restated)
                .collect();
            let held = rank.map_or(0, |r| {
                part.iter()
                    .filter(|id| subjects[r].0.contains(**id))
                    .count()
            });
            held as f64 / part.len().max(1) as f64
        };
        let name = rank.and_then(|r| subjects[r].1.clone());
        let word = invented(&core.subject).to_lowercase();
        piles.push(PileResult {
            name: core.subject.clone(),
            class: class_of(i),
            size: pile.len(),
            coverage,
            coverage_restated: share(true),
            coverage_aspects: share(false),
            purity,
            recovered: coverage >= FOUND && purity >= FOUND,
            named: name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains(&word)),
            subject_name: name,
            rank: rank.map(|r| r + 1),
            worth: rank.map(|r| subjects[r].2),
        });
    }
    let background_subjects = subjects
        .iter()
        .filter(|(ids, _, _)| {
            !members
                .iter()
                .any(|p| ids.intersection(p).count() * 2 > ids.len())
        })
        .count();

    // C: a pair from different classes is right when the earlier class
    // ranks higher (a missing subject ranks last).
    let (mut pairs, mut right) = (0usize, 0usize);
    for a in &piles {
        for b in &piles {
            if a.class < b.class {
                pairs += 1;
                let ra = a.rank.unwrap_or(usize::MAX);
                let rb = b.rank.unwrap_or(usize::MAX);
                right += usize::from(ra < rb);
            }
        }
    }

    // D: the islands view, everything.
    let found: HashSet<String> = engine
        .islands_page(false, 0, usize::MAX)?
        .islands
        .into_iter()
        .map(|i| i.id)
        .collect();
    let hit = found.intersection(&islands).count();

    let k = piles.len().max(1) as f64;
    let matched: Vec<&PileResult> = piles.iter().filter(|p| p.rank.is_some()).collect();
    Ok(SizeReport {
        notes: written,
        band: page.band,
        min_size: page.min_size,
        max_size: page.max_size,
        subjects: subjects.len(),
        background_subjects,
        recovered: piles.iter().filter(|p| p.recovered).count() as f64 / k,
        mean_purity: matched.iter().map(|p| p.purity).sum::<f64>() / matched.len().max(1) as f64,
        named: piles.iter().filter(|p| p.named).count() as f64 / k,
        worth_order: right as f64 / pairs.max(1) as f64,
        island_recall: hit as f64 / islands.len().max(1) as f64,
        island_precision: hit as f64 / found.len().max(1) as f64,
        piles,
        millis: started.elapsed().as_millis(),
    })
}

pub fn run(cfg: &Config) -> anyhow::Result<SleepReport> {
    let (_, name) = embedder(cfg.embed_model.as_deref());
    let fake = !cfg!(feature = "fastembed");
    let mut sizes = Vec::new();
    for &n in &cfg.sizes {
        sizes.push(run_size(cfg, n)?);
    }
    Ok(SleepReport {
        embedder: name,
        embeddings_are_fake: fake,
        seed: cfg.seed,
        sizes,
    })
}

pub fn print(r: &SleepReport) {
    println!("sleep bench — embedder {}, seed {}", r.embedder, r.seed);
    if r.embeddings_are_fake {
        println!("!! fake embeddings: crowding is noise. Build with --features fastembed.");
    }
    for s in &r.sizes {
        println!(
            "\n── {} notes  band {:.3}  subjects {} (min {}, max {}), {} background-only  {} ms",
            s.notes, s.band, s.subjects, s.min_size, s.max_size, s.background_subjects, s.millis
        );
        println!(
            "   A recovered {:.2}  purity {:.2}   B named {:.2}   C worth order {:.2}   D islands recall {:.2} precision {:.2}",
            s.recovered, s.mean_purity, s.named, s.worth_order, s.island_recall, s.island_precision
        );
        for p in &s.piles {
            println!(
                "   {:<14} {:>3} notes  cover {:.2} (restated {:.2}, aspects {:.2}) pure {:.2}  rank {:>3}  {:<40} {}",
                format!("{:?}", p.class),
                p.size,
                p.coverage,
                p.coverage_restated,
                p.coverage_aspects,
                p.purity,
                p.rank.map_or("-".into(), |r| r.to_string()),
                p.subject_name.as_deref().unwrap_or("(none)"),
                p.name,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piles_are_about_their_subject_and_sized() {
        let c = corpus_full(
            40,
            0,
            7,
            &Default::default(),
            &crate::generate::DEFAULT_TYPE_MIX,
        );
        let mut facts = c.facts.into_iter();
        let core = facts.next().unwrap();
        let nodes = pile(&core, &mut facts, 12);
        assert_eq!(nodes.len(), 12);
        assert_eq!(nodes.iter().filter(|(_, r)| *r).count(), 6);
        let word = invented(&core.subject);
        assert!(nodes.iter().all(|(n, _)| n.title.contains(word)), "{word}");
        let titles: HashSet<&str> = nodes.iter().map(|(n, _)| n.title.as_str()).collect();
        assert_eq!(titles.len(), 12, "no two notes alike");
    }
}
