//! Sleep's reading side (0.11.0): the crowding index over this graph, the
//! review queue of subjects it cuts (see [`crate::crowding`]), ranked by
//! how much they are worth consolidating, one note's neighbourhood, the
//! conflicts to settle (standing `conflicts-with` edges beside pending
//! suspects), the islands (notes no edge and no tag reaches), and reviewed
//! marks.
//! Nominations only — nothing here writes a node, an edge, or trust; the
//! one write is the reviewed mark, which lives in the store's kv beside the
//! graph (older binaries never read the key, so it costs them nothing).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;

use super::{Engine, is_anchor, is_tombstone};
use crate::crowding::{CrowdIndex, fnv};
use crate::{Node, NodeType, Result, SuspectView};

/// The kv key holding reviewed marks: a JSON object {node id: content
/// fingerprint in hex}.
const MARKS_KEY: &str = "review.marks";
/// Body characters a review member carries; `get_node` reads the rest.
const SNIPPET: usize = 240;
/// Edges listed per member / per `around` view.
const MEMBER_EDGES: usize = 4;
const AROUND_EDGES: usize = 20;
/// Neighbours and subject-mates an `around` view lists.
const AROUND_NEAR: usize = 12;
/// Members a subject shows in full; the rest come as id + title lines.
const SHOWN: usize = 12;
/// Closest member pairs a subject lists — the merge candidates.
const PAIRS: usize = 12;
/// A member younger than this is live work, not history to fold.
const SETTLED_DAYS: i64 = 14;
/// Nearest notes an island lists.
const ISLAND_NEAR: usize = 3;
/// Characters of a linked note's title a member's edge carries.
const EDGE_TITLE: usize = 80;

/// "v0.7.0" and "0.7.0" are one version.
fn bare(v: &str) -> &str {
    v.trim_start_matches(['v', 'V'])
}

/// When each working version began, from the `set_version` journal: a note
/// captured before version stamping (or with it off) gets the version that
/// was current when it was written. Notes older than the journal share one
/// label, "<first", so a subject's history before it still counts once.
struct VersionClock(Vec<(i64, String)>);

impl VersionClock {
    fn version_of(&self, n: &Node) -> Option<String> {
        if let Some(v) = n.version.as_deref() {
            return Some(bare(v).to_string());
        }
        let first = self.0.first()?;
        Some(
            self.0
                .iter()
                .rev()
                .find(|(ts, _)| *ts <= n.created_at)
                .map_or_else(|| format!("<{}", first.1), |(_, v)| v.clone()),
        )
    }
}

/// Oldest first; "<0.7.0" sorts before every real version.
fn version_order(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |v: &str| (!v.starts_with('<'), v.trim_start_matches('<').to_string());
    let (ka, kb) = (key(a), key(b));
    ka.0.cmp(&kb.0)
        .then_with(|| crate::tasks::version_cmp(&ka.1, &kb.1).unwrap_or_else(|| ka.1.cmp(&kb.1)))
}

/// One note as the review sees it — enough to judge "same claim, merge" vs
/// "same subject, keep" without a get_node per member.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewMember {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    /// The body's opening (`get_node` has the whole).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub durability: crate::Durability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::NodeStatus>,
    pub trust: f64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Close neighbours in this graph (see [`crate::crowding`]).
    pub crowding: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reviewed: bool,
    /// Cosine to the subject's most central member (absent on it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<ReviewEdge>,
    /// Live edges beyond the listed ones.
    #[serde(skip_serializing_if = "is_zero")]
    pub more_edges: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewEdge {
    pub verb: String,
    /// "out" = this note → other; "in" = other → this note.
    pub dir: &'static str,
    pub id: String,
    pub title: String,
}

/// Two members and how close they are.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewPair {
    pub a: String,
    pub b: String,
    pub similarity: f32,
    /// The verb already joining them, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked: Option<String>,
}

/// A member past the [`SHOWN`] ones: enough to name it.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewLine {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Why a subject ranks where it does: big, spread over many releases, and
/// settled (old, from past versions) is history worth folding into one
/// state-of note; a pile of this cycle's notes is live work.
#[derive(Debug, Clone, Serialize)]
pub struct Worth {
    /// size × (1 + ln versions) × (0.3 + 0.7 × settled), one decimal.
    pub score: f64,
    /// Distinct versions the members were captured in (stamped, or read
    /// off the `set_version` journal by creation time).
    pub versions: usize,
    /// Share of members from a past version and older than two weeks.
    pub settled: f64,
    /// Days between the oldest and the newest member.
    pub span_days: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewSubject {
    /// What it is about, in two or three terms (titles and tags, ranked
    /// by how much rarer they are here than in the rest of the graph).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub size: usize,
    /// The cosine this subject holds together at (see `crowding`).
    pub cohesion: f32,
    pub worth: Worth,
    /// Anchors or tags most members share — what the subject is about.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared: Vec<String>,
    /// Oldest and newest member version; an unstamped note counts in the
    /// version that was current when it was written ("<0.7.0": before the
    /// graph's first `set_version`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versions: Option<(String, String)>,
    /// The most central members, in full.
    pub members: Vec<ReviewMember>,
    /// The rest, by centrality.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<ReviewLine>,
    /// The closest member pairs, closest first: where a merge may hide.
    pub pairs: Vec<ReviewPair>,
    /// Pending suspect ids between members (judge with resolve_suspect).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suspects: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewPage {
    /// The cosine this graph calls close (its own top 1% of pairs).
    pub band: f32,
    /// A subject's size bounds on this graph (2% of its notes, ×4).
    pub min_size: usize,
    pub max_size: usize,
    /// Subjects matching the filters, before paging.
    pub total: usize,
    /// Subjects left out because every member carries a current mark.
    pub hidden_reviewed: usize,
    pub offset: usize,
    /// Most worth consolidating first.
    pub subjects: Vec<ReviewSubject>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Neighbor {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    pub similarity: f32,
    /// Inside this graph's close band.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub close: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked: Option<String>,
    /// What it shares with the note (`anchor:<title>`, `tag:<t>`, `ref:<path>`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AroundView {
    pub band: f32,
    pub node: ReviewMember,
    /// Nearest notes by embedding, no search cut-off.
    pub nearest: Vec<Neighbor>,
    /// Notes sharing an anchor, tag, or code ref, closest first.
    pub subject: Vec<Neighbor>,
}

/// A standing contradiction edge, with both ends.
#[derive(Debug, Clone, Serialize)]
pub struct StandingConflict {
    /// The edge id (`update_edge` resolves it; a `replaces` link between
    /// the two settles it for good).
    pub edge: String,
    pub a: ReviewLine,
    pub b: ReviewLine,
    /// Which side was written later: "a" or "b".
    pub newer: &'static str,
    pub since: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Sleep's first queue: what disagrees and has not been settled.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictsView {
    /// Contradiction edges still active, newest first.
    pub standing: Vec<StandingConflict>,
    /// Look-alike pairs the scan flagged, not yet judged (`resolve_suspect`).
    pub suspects: Vec<SuspectView>,
}

/// A note no live edge and no tag reaches — only text search finds it.
#[derive(Debug, Clone, Serialize)]
pub struct Island {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Its nearest notes by embedding: where a link would go.
    pub nearest: Vec<Neighbor>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IslandPage {
    pub total: usize,
    pub hidden_reviewed: usize,
    pub offset: usize,
    /// Closest neighbour first: the easiest to tie back in lead.
    pub islands: Vec<Island>,
}

/// What the review queue admits.
#[derive(Debug, Clone, Default)]
pub struct ReviewQuery {
    pub types: Vec<NodeType>,
    pub tag: Option<String>,
    pub include_reviewed: bool,
    pub offset: usize,
    pub limit: usize,
}

/// One note's crowding for the pane's heat view.
#[derive(Debug, Clone, Serialize)]
pub struct CrowdHeat {
    pub crowding: usize,
    pub heat: f64,
}

/// A subject as the pane lists it.
#[derive(Debug, Clone, Serialize)]
pub struct SubjectSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Most central first.
    pub members: Vec<String>,
    pub worth: Worth,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrowdingReport {
    pub band: f32,
    pub size: usize,
    pub min_size: usize,
    pub nodes: HashMap<String, CrowdHeat>,
    /// Unreviewed subjects, most worth consolidating first.
    pub subjects: Vec<SubjectSummary>,
    pub hidden_reviewed: usize,
    /// Unreviewed islands: notes no edge and no tag reaches.
    pub islands: Vec<String>,
}

/// A note's content fingerprint — what a reviewed mark vouches for.
fn content_fp(n: &Node) -> u64 {
    fnv(&[
        n.node_type.as_str().as_bytes(),
        n.title.as_bytes(),
        n.body.as_deref().unwrap_or("").as_bytes(),
    ])
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut ab, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0.0 || bb == 0.0 {
        0.0
    } else {
        ab / (aa.sqrt() * bb.sqrt())
    }
}

fn snippet(body: Option<&str>) -> String {
    let body = body.unwrap_or("").trim();
    match body.char_indices().nth(SNIPPET) {
        Some((at, _)) => format!("{}…", &body[..at]),
        None => body.to_string(),
    }
}

impl Engine {
    /// The crowding index over the live, scannable notes (no anchors, no
    /// tombstones — the same set the conflict scan reads). Cached until a
    /// note's text, the note set, or the embedder changes.
    pub fn crowding(&self) -> Result<Arc<CrowdIndex>> {
        let nodes = self.store.scannable_nodes()?;
        let model = self.embed_model_id();
        let mut parts: Vec<&[u8]> = vec![model.name.as_bytes()];
        let dim = model.dim.to_le_bytes();
        parts.push(&dim);
        for n in &nodes {
            parts.push(n.id.as_bytes());
            parts.push(n.title.as_bytes());
            parts.push(n.body.as_deref().unwrap_or("").as_bytes());
        }
        let fingerprint = fnv(&parts);
        if let Some(hit) = self
            .crowd_cache
            .lock()
            .unwrap()
            .as_ref()
            .filter(|c| c.fingerprint == fingerprint)
        {
            return Ok(hit.clone());
        }
        let mut items = Vec::with_capacity(nodes.len());
        for n in &nodes {
            if let Some(v) = self.store.embedding_of(&n.id)? {
                items.push((n.id.clone(), v));
            }
        }
        let index = Arc::new(CrowdIndex::build(&items, fingerprint));
        *self.crowd_cache.lock().unwrap() = Some(index.clone());
        Ok(index)
    }

    fn review_marks(&self) -> Result<HashMap<String, String>> {
        Ok(self
            .store
            .kv_get(MARKS_KEY)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default())
    }

    fn is_reviewed(marks: &HashMap<String, String>, n: &Node) -> bool {
        marks
            .get(&n.id)
            .is_some_and(|fp| *fp == format!("{:016x}", content_fp(n)))
    }

    /// Mark notes reviewed (or clear the marks with `reviewed: false`). A
    /// mark vouches for the note's current type, title, and body; any edit
    /// voids it. It touches nothing on the node — not confirmed_at, not
    /// trust: "looked at during a sweep" is not "verified still true".
    /// Returns the ids that were marked (unknown ids are skipped).
    pub fn mark_reviewed(&self, ids: &[String], reviewed: bool) -> Result<Vec<String>> {
        let mut marks = self.review_marks()?;
        let mut done = Vec::new();
        for id in ids {
            let Some(n) = self.store.get_node(id)? else {
                continue;
            };
            if reviewed {
                marks.insert(n.id.clone(), format!("{:016x}", content_fp(&n)));
            } else {
                marks.remove(&n.id);
            }
            done.push(n.id);
        }
        // Prune marks for notes that are gone or archived, so the key never
        // outgrows the live graph.
        let live: HashSet<String> = self
            .store
            .scannable_nodes()?
            .into_iter()
            .map(|n| n.id)
            .collect();
        marks.retain(|id, _| live.contains(id));
        self.store
            .kv_set(MARKS_KEY, &serde_json::to_string(&marks)?)?;
        Ok(done)
    }

    fn review_member(
        &self,
        n: &Node,
        index: &CrowdIndex,
        marks: &HashMap<String, String>,
        edge_cap: usize,
    ) -> Result<ReviewMember> {
        let mut edges = Vec::new();
        let mut total = 0usize;
        let live = |e: &crate::Edge| e.valid_until.is_none();
        let outs = self.store.edges_out(&n.id)?;
        let ins = self.store.edges_in(&n.id)?;
        for (dir, e, other) in outs
            .iter()
            .filter(|e| live(e))
            .map(|e| ("out", e, &e.to_id))
            .chain(
                ins.iter()
                    .filter(|e| live(e))
                    .map(|e| ("in", e, &e.from_id)),
            )
        {
            total += 1;
            if edges.len() >= edge_cap {
                continue;
            }
            let title = self
                .store
                .get_node(other)?
                .map(|o| crate::harvest::truncate_words(&o.title, EDGE_TITLE))
                .unwrap_or_default();
            edges.push(ReviewEdge {
                verb: e.edge_type.as_str().to_string(),
                dir,
                id: other.clone(),
                title,
            });
        }
        Ok(ReviewMember {
            id: n.id.clone(),
            node_type: n.node_type.clone(),
            title: n.title.clone(),
            snippet: snippet(n.body.as_deref()),
            version: n.version.clone(),
            durability: n.durability,
            status: n.status,
            trust: (n.trust * 100.0).round() / 100.0,
            stale: n.stale,
            pinned: n.trust_override.is_some(),
            created_at: n.created_at,
            tags: n.tags.clone(),
            crowding: index.nodes.get(&n.id).map_or(0, |c| c.crowding),
            reviewed: Self::is_reviewed(marks, n),
            similarity: None,
            more_edges: total - edges.len(),
            edges,
        })
    }

    /// The verb joining two notes either way, if any live edge does.
    fn link_between(&self, a: &str, b: &str) -> Result<Option<String>> {
        Ok(self
            .store
            .edges_out(a)?
            .into_iter()
            .chain(self.store.edges_in(a)?)
            .find(|e| e.valid_until.is_none() && (e.to_id == b || e.from_id == b))
            .map(|e| e.edge_type.as_str().to_string()))
    }

    /// Anchors a note is `about` (any out-edge into an anchor-role node).
    fn anchors_of(&self, id: &str) -> Result<Vec<(String, String)>> {
        let cfg = self.store.config();
        let mut out = Vec::new();
        for e in self.store.edges_out(id)? {
            if e.valid_until.is_some() {
                continue;
            }
            if let Some(t) = self.store.get_node(&e.to_id)?
                && is_anchor(&cfg, &t)
            {
                out.push((t.id, t.title));
            }
        }
        Ok(out)
    }

    /// The working-version switches, oldest first (see [`VersionClock`]).
    fn version_clock(&self) -> Result<VersionClock> {
        let page = self.store.audit_page(None, Some("version"), 4096)?;
        let mut switches: Vec<(i64, String)> = page
            .entries
            .iter()
            .filter(|r| r.action == "version_switched")
            .filter_map(|r| {
                let (_, next) = r.title.as_deref()?.split_once('→')?;
                let next = bare(next.trim());
                (next != "(unset)").then(|| (r.ts, next.to_string()))
            })
            .collect();
        switches.sort_by_key(|(ts, _)| *ts);
        Ok(VersionClock(switches))
    }

    /// How much a subject is worth consolidating (see [`Worth`]).
    fn worth(&self, members: &[&Node], working: Option<&str>, clock: &VersionClock) -> Worth {
        let now = crate::now();
        let day = 86_400;
        let stamped: Vec<Option<String>> = members.iter().map(|n| clock.version_of(n)).collect();
        let versions: HashSet<&str> = stamped.iter().flatten().map(String::as_str).collect();
        let working = working.map(bare);
        let settled = members
            .iter()
            .zip(&stamped)
            .filter(|(n, v)| v.as_deref() != working && now - n.created_at > SETTLED_DAYS * day)
            .count() as f64
            / members.len().max(1) as f64;
        let (lo, hi) = members.iter().fold((i64::MAX, i64::MIN), |(lo, hi), n| {
            (lo.min(n.created_at), hi.max(n.created_at))
        });
        let score = members.len() as f64
            * (1.0 + (versions.len().max(1) as f64).ln())
            * (0.3 + 0.7 * settled);
        Worth {
            score: (score * 10.0).round() / 10.0,
            versions: versions.len(),
            settled: (settled * 100.0).round() / 100.0,
            span_days: if members.is_empty() {
                0
            } else {
                (hi - lo) / day
            },
        }
    }

    /// Subjects the filters admit, most worth first, split into (shown,
    /// hidden-as-reviewed), each with its worth and name.
    #[allow(clippy::type_complexity)]
    fn review_subjects(
        &self,
        index: &CrowdIndex,
        q: &ReviewQuery,
    ) -> Result<(
        Vec<(crate::crowding::Subject, Worth, Option<String>)>,
        usize,
        HashMap<String, Node>,
    )> {
        let marks = self.review_marks()?;
        let working = self.current_version()?;
        let nodes: HashMap<String, Node> = self
            .store
            .scannable_nodes()?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        let admit = |id: &str| {
            nodes.get(id).is_some_and(|n| {
                (q.types.is_empty() || q.types.contains(&n.node_type))
                    && q.tag.as_ref().is_none_or(|t| n.tags.contains(t))
            })
        };
        let terms = crate::naming::TermIndex::build(nodes.values());
        let clock = self.version_clock()?;
        let mut hidden = 0;
        let mut out = Vec::new();
        for subject in index.subjects(admit) {
            let members: Vec<&Node> = subject
                .members
                .iter()
                .filter_map(|m| nodes.get(m))
                .collect();
            // Reviewed when most of it is: a worked subject re-forms around
            // its state-of note and a stray or two, and comes back only once
            // new or changed notes outnumber the reviewed ones.
            let reviewed = members
                .iter()
                .filter(|n| Self::is_reviewed(&marks, n))
                .count();
            let done = reviewed * 2 > members.len();
            if done && !q.include_reviewed {
                hidden += 1;
                continue;
            }
            let worth = self.worth(&members, working.as_deref(), &clock);
            let name = terms.name(&members);
            out.push((subject, worth, name));
        }
        out.sort_by(|a, b| {
            b.1.score
                .total_cmp(&a.1.score)
                .then(b.0.members.len().cmp(&a.0.members.len()))
        });
        Ok((out, hidden, nodes))
    }

    /// One page of the review queue: subjects, most worth consolidating
    /// first, each with what it takes to judge summarize / merge / keep.
    pub fn review_page(&self, q: &ReviewQuery) -> Result<ReviewPage> {
        let index = self.crowding()?;
        let bounds = index.subject_bounds();
        let marks = self.review_marks()?;
        let (subjects, hidden, nodes) = self.review_subjects(&index, q)?;
        let total = subjects.len();
        let pending = self.store.suspects_pending()?;
        let clock = self.version_clock()?;
        let mut page = Vec::new();
        for (subject, worth, name) in subjects.into_iter().skip(q.offset).take(q.limit) {
            let all: Vec<&Node> = subject
                .members
                .iter()
                .filter_map(|m| nodes.get(m))
                .collect();
            let vectors: Vec<Vec<f32>> = all
                .iter()
                .map(|n| Ok(self.store.embedding_of(&n.id)?.unwrap_or_default()))
                .collect::<Result<_>>()?;
            let mut members = Vec::new();
            for (i, n) in all.iter().take(SHOWN).enumerate() {
                let mut m = self.review_member(n, &index, &marks, MEMBER_EDGES)?;
                if i > 0 {
                    m.similarity =
                        Some((cosine(&vectors[0], &vectors[i]) * 1000.0).round() / 1000.0);
                }
                members.push(m);
            }
            let more = all
                .iter()
                .skip(SHOWN)
                .map(|n| ReviewLine {
                    id: n.id.clone(),
                    node_type: n.node_type.clone(),
                    title: n.title.clone(),
                    version: n.version.clone(),
                })
                .collect();
            let mut close = Vec::new();
            for i in 0..all.len() {
                for j in (i + 1)..all.len() {
                    let s = cosine(&vectors[i], &vectors[j]);
                    if s >= index.band {
                        close.push((i, j, s));
                    }
                }
            }
            close.sort_by(|x, y| y.2.total_cmp(&x.2));
            let mut pairs = Vec::new();
            for (i, j, s) in close.into_iter().take(PAIRS) {
                pairs.push(ReviewPair {
                    a: all[i].id.clone(),
                    b: all[j].id.clone(),
                    similarity: (s * 1000.0).round() / 1000.0,
                    linked: self.link_between(&all[i].id, &all[j].id)?,
                });
            }
            // What they are about: anchors and tags at least a third share.
            let mut counts: HashMap<String, usize> = HashMap::new();
            for n in &all {
                for (_, title) in self.anchors_of(&n.id)? {
                    *counts.entry(format!("anchor:{title}")).or_default() += 1;
                }
                for t in &n.tags {
                    *counts.entry(format!("tag:{t}")).or_default() += 1;
                }
            }
            let third = all.len().div_ceil(3).max(2);
            let mut shared: Vec<(String, usize)> =
                counts.into_iter().filter(|(_, c)| *c >= third).collect();
            shared.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let mut stamped: Vec<String> = all.iter().filter_map(|n| clock.version_of(n)).collect();
            stamped.sort_by(|a, b| version_order(a, b));
            let versions = stamped
                .first()
                .zip(stamped.last())
                .map(|(a, b)| (a.clone(), b.clone()));
            let ids: HashSet<&str> = all.iter().map(|n| n.id.as_str()).collect();
            let suspects = pending
                .iter()
                .filter(|s| ids.contains(s.a.id.as_str()) && ids.contains(s.b.id.as_str()))
                .map(|s| s.id.clone())
                .collect();
            page.push(ReviewSubject {
                name,
                size: all.len(),
                cohesion: (subject.cohesion * 1000.0).round() / 1000.0,
                worth,
                shared: shared.into_iter().map(|(s, _)| s).collect(),
                versions,
                members,
                more,
                pairs,
                suspects,
            });
        }
        Ok(ReviewPage {
            band: index.band,
            min_size: bounds.min,
            max_size: bounds.max,
            total,
            hidden_reviewed: hidden,
            offset: q.offset,
            subjects: page,
        })
    }

    /// One note's neighbourhood: its nearest notes by embedding (no search
    /// cut-off) and the notes sharing its anchors, tags, or code refs. An
    /// anchor's own view lists the notes about it.
    pub fn around(&self, id: &str) -> Result<AroundView> {
        let Some(node) = self.store.get_node(id)? else {
            return Err(crate::Error::NotFound(id.to_string()));
        };
        let cfg = self.store.config();
        let index = self.crowding()?;
        let marks = self.review_marks()?;
        let me = self.review_member(&node, &index, &marks, AROUND_EDGES)?;
        let my_vec = self.store.embedding_of(id)?;
        let neighbor = |other: &Node, similarity: f32, via: Vec<String>| -> Result<Neighbor> {
            Ok(Neighbor {
                id: other.id.clone(),
                node_type: other.node_type.clone(),
                title: other.title.clone(),
                similarity: (similarity * 1000.0).round() / 1000.0,
                close: similarity >= index.band,
                linked: self.link_between(id, &other.id)?,
                via,
            })
        };
        let mut nearest = Vec::new();
        if let Some(crowd) = index.nodes.get(id) {
            for (other, s) in crowd.nearest.iter().take(AROUND_NEAR) {
                if let Some(o) = self.store.get_node(other)? {
                    nearest.push(neighbor(&o, *s, Vec::new())?);
                }
            }
        }
        // Subject-mates: what the note is about, its tags, its code refs.
        let mut via: HashMap<String, Vec<String>> = HashMap::new();
        let anchors: Vec<(String, String)> = if is_anchor(&cfg, &node) {
            vec![(node.id.clone(), node.title.clone())]
        } else {
            self.anchors_of(id)?
        };
        for (anchor, title) in &anchors {
            for e in self.store.edges_in(anchor)? {
                if e.valid_until.is_none() && e.from_id != id {
                    via.entry(e.from_id.clone())
                        .or_default()
                        .push(format!("anchor:{title}"));
                }
            }
        }
        if !node.tags.is_empty() || !node.code_refs.is_empty() {
            for other in self.store.scannable_nodes()? {
                if other.id == id {
                    continue;
                }
                for t in other.tags.iter().filter(|t| node.tags.contains(t)) {
                    via.entry(other.id.clone())
                        .or_default()
                        .push(format!("tag:{t}"));
                }
                for r in other
                    .code_refs
                    .iter()
                    .filter(|r| node.code_refs.contains(r))
                {
                    via.entry(other.id.clone())
                        .or_default()
                        .push(format!("ref:{r}"));
                }
            }
        }
        let mut subject = Vec::new();
        for (other, why) in via {
            let Some(o) = self.store.get_node(&other)? else {
                continue;
            };
            if o.valid_until.is_some() || is_tombstone(&cfg, &o) {
                continue;
            }
            let s = match (&my_vec, self.store.embedding_of(&other)?) {
                (Some(a), Some(b)) => cosine(a, &b),
                _ => 0.0,
            };
            subject.push((o, s, why));
        }
        // More shared ties first, then closeness.
        subject.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then(b.1.total_cmp(&a.1)));
        let subject = subject
            .into_iter()
            .take(AROUND_NEAR)
            .map(|(o, s, why)| neighbor(&o, s, why))
            .collect::<Result<Vec<_>>>()?;
        Ok(AroundView {
            band: index.band,
            node: me,
            nearest,
            subject,
        })
    }

    /// Everything the pane's heat view and Checkup section need.
    pub fn crowding_report(&self) -> Result<CrowdingReport> {
        let index = self.crowding()?;
        let (subjects, hidden, _) = self.review_subjects(
            &index,
            &ReviewQuery {
                limit: usize::MAX,
                ..Default::default()
            },
        )?;
        Ok(CrowdingReport {
            band: index.band,
            size: index.size,
            min_size: index.subject_bounds().min,
            nodes: index
                .nodes
                .iter()
                .map(|(id, c)| {
                    (
                        id.clone(),
                        CrowdHeat {
                            crowding: c.crowding,
                            heat: c.heat,
                        },
                    )
                })
                .collect(),
            subjects: subjects
                .into_iter()
                .map(|(s, worth, name)| SubjectSummary {
                    name,
                    members: s.members,
                    worth,
                })
                .collect(),
            hidden_reviewed: hidden,
            islands: self
                .island_notes(false)?
                .0
                .into_iter()
                .map(|n| n.id)
                .collect(),
        })
    }

    /// Standing contradiction edges and pending suspects, for a sleep's
    /// first pass.
    pub fn conflicts_view(&self) -> Result<ConflictsView> {
        let line = |n: &Node| ReviewLine {
            id: n.id.clone(),
            node_type: n.node_type.clone(),
            title: n.title.clone(),
            version: n.version.clone(),
        };
        let mut standing = Vec::new();
        for e in self.store.active_conflict_edges()? {
            let (Some(a), Some(b)) = (
                self.store.get_node(&e.from_id)?,
                self.store.get_node(&e.to_id)?,
            ) else {
                continue;
            };
            standing.push(StandingConflict {
                edge: e.id.clone(),
                newer: if b.created_at > a.created_at {
                    "b"
                } else {
                    "a"
                },
                a: line(&a),
                b: line(&b),
                since: e.created_at,
                note: e.note.clone(),
            });
        }
        Ok(ConflictsView {
            standing,
            suspects: self.suspects()?,
        })
    }

    /// Live notes no live edge and no tag reaches, unreviewed ones only
    /// unless asked, each with its nearest notes.
    fn island_notes(&self, include_reviewed: bool) -> Result<(Vec<Node>, usize)> {
        let mut linked: HashSet<String> = HashSet::new();
        for e in self.store.all_edges()? {
            if e.valid_until.is_none() {
                linked.insert(e.from_id);
                linked.insert(e.to_id);
            }
        }
        let marks = self.review_marks()?;
        let mut hidden = 0;
        let mut out = Vec::new();
        for n in self.store.scannable_nodes()? {
            if linked.contains(&n.id) || !n.tags.is_empty() {
                continue;
            }
            if !include_reviewed && Self::is_reviewed(&marks, &n) {
                hidden += 1;
                continue;
            }
            out.push(n);
        }
        Ok((out, hidden))
    }

    /// One page of islands, the closest to a neighbour first.
    pub fn islands_page(
        &self,
        include_reviewed: bool,
        offset: usize,
        limit: usize,
    ) -> Result<IslandPage> {
        let index = self.crowding()?;
        let (notes, hidden) = self.island_notes(include_reviewed)?;
        let closest = |id: &str| {
            index
                .nodes
                .get(id)
                .and_then(|c| c.nearest.first())
                .map_or(0.0, |(_, s)| *s)
        };
        let mut notes = notes;
        notes.sort_by(|a, b| {
            closest(&b.id)
                .total_cmp(&closest(&a.id))
                .then(a.id.cmp(&b.id))
        });
        let total = notes.len();
        let mut islands = Vec::new();
        for n in notes.into_iter().skip(offset).take(limit) {
            let mut nearest = Vec::new();
            if let Some(c) = index.nodes.get(&n.id) {
                for (other, s) in c.nearest.iter().take(ISLAND_NEAR) {
                    if let Some(o) = self.store.get_node(other)? {
                        nearest.push(Neighbor {
                            id: o.id.clone(),
                            node_type: o.node_type.clone(),
                            title: o.title.clone(),
                            similarity: (s * 1000.0).round() / 1000.0,
                            close: *s >= index.band,
                            linked: None,
                            via: Vec::new(),
                        });
                    }
                }
            }
            islands.push(Island {
                snippet: snippet(n.body.as_deref()),
                id: n.id,
                node_type: n.node_type,
                title: n.title,
                version: n.version,
                nearest,
            });
        }
        Ok(IslandPage {
            total,
            hidden_reviewed: hidden,
            offset,
            islands,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Durability, EdgeType, Embedder, NewEdge, NewNode, NodePatch, Source, SqliteStore};

    const DIM: usize = 384;

    /// Vectors with subjects: a `#word` marker puts the text on that word's
    /// axis, and the whole text adds a smaller axis of its own — same-subject
    /// notes sit at ~0.92, everything else near 0. Not fake, so the brief
    /// treats it as real.
    struct TopicEmbedder;

    fn axis(text: &str) -> usize {
        (fnv(&[text.as_bytes()]) % DIM as u64) as usize
    }

    impl Embedder for TopicEmbedder {
        fn dim(&self) -> usize {
            DIM
        }
        fn name(&self) -> &str {
            "topic-test"
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![0.0f32; DIM];
                    if let Some(tag) = t.split_whitespace().find(|w| w.starts_with('#')) {
                        v[axis(tag)] += 1.0;
                    }
                    // Spread over three axes, so two strangers that share
                    // one (384 axes, birthday odds) stay far apart.
                    for salt in ["a", "b", "c"] {
                        v[axis(&format!("{salt}{t}"))] += 0.3 / 3f32.sqrt();
                    }
                    v
                })
                .collect())
        }
    }

    fn engine() -> Engine {
        Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(TopicEmbedder),
        )
    }

    fn add(e: &Engine, title: &str) -> Node {
        e.add_node(NewNode {
            node_type: NodeType::Decision,
            title: title.to_string(),
            body: Some(format!("{title} — body text")),
            created_at: None,
            durability: Durability::Stable,
            source: Source::Claude,
            session_id: None,
            status: None,
            code_refs: vec![],
            tags: vec![],
            version: None,
            props: None,
            fields: None,
        })
        .unwrap()
    }

    /// Six notes on #bridge, five on #brief, and 30 strangers: 41 notes,
    /// so a subject needs 5 (the floor; 2% would be 1).
    fn crowded() -> (Engine, Vec<Node>, Vec<Node>) {
        let e = engine();
        let bridge: Vec<Node> = (0..6)
            .map(|i| add(&e, &format!("#bridge note {i} on reconnects")))
            .collect();
        let brief: Vec<Node> = (0..5)
            .map(|i| add(&e, &format!("#brief note {i} on sections")))
            .collect();
        for i in 0..30 {
            add(&e, &format!("stranger {i} about something else entirely"));
        }
        (e, bridge, brief)
    }

    fn query() -> ReviewQuery {
        ReviewQuery {
            limit: 10,
            ..Default::default()
        }
    }

    fn ids(nodes: &[Node]) -> Vec<String> {
        let mut v: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        v.sort();
        v
    }

    fn member_ids(c: &ReviewSubject) -> Vec<String> {
        let mut v: Vec<String> = c.members.iter().map(|m| m.id.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn the_review_queue_lists_subjects_most_worth_first() {
        let (e, bridge, brief) = crowded();
        let page = e.review_page(&query()).unwrap();
        assert_eq!((page.min_size, page.max_size), (5, 20));
        assert_eq!(page.total, 2, "two piles, no stranger subjects");
        assert_eq!(member_ids(&page.subjects[0]), ids(&bridge));
        assert_eq!(member_ids(&page.subjects[1]), ids(&brief));
        let first = &page.subjects[0];
        assert_eq!(first.size, 6);
        assert_eq!(
            first.pairs.len(),
            PAIRS,
            "15 close pairs, the closest 12 listed"
        );
        assert!(first.more.is_empty());
        assert!(first.members[0].similarity.is_none(), "the centre");
        assert!(
            first.members[1..]
                .iter()
                .all(|m| m.similarity.unwrap() > 0.9)
        );
        assert!(first.members.iter().all(|m| m.crowding == 5));
        assert!(first.members[0].snippet.contains("body text"));
        // All of this cycle, all new: live work, worth only its size × 0.3.
        assert_eq!(first.worth.settled, 0.0);
        assert_eq!(first.worth.score, 1.8);
    }

    #[test]
    fn settled_history_outranks_a_bigger_live_pile() {
        let e = engine();
        e.set_current_version(Some("0.3.0")).unwrap();
        let month_ago = crate::now() - 40 * 86_400;
        for (i, v) in ["0.1.0", "0.1.0", "0.2.0", "0.2.0", "0.2.1"]
            .iter()
            .enumerate()
        {
            let title = format!("#cutover note {i} on the store move");
            e.add_node(NewNode {
                created_at: Some(month_ago + i as i64 * 5 * 86_400),
                version: Some(v.to_string()),
                body: Some(format!("{title} — body text")),
                title,
                ..new_like(&add(&e, "seed stranger for the shape"))
            })
            .unwrap();
        }
        for i in 0..8 {
            add(&e, &format!("#cycle note {i} on this release"));
        }
        for i in 0..30 {
            add(&e, &format!("stranger {i} about something else entirely"));
        }
        let page = e.review_page(&query()).unwrap();
        let top = &page.subjects[0];
        assert!(
            top.members.iter().all(|m| m.title.contains("#cutover")),
            "{:?}",
            top.members[0].title
        );
        assert_eq!(top.worth.versions, 3);
        assert_eq!(top.worth.span_days, 20);
        assert_eq!(top.versions, Some(("0.1.0".into(), "0.2.1".into())));
        let live = page
            .subjects
            .iter()
            .find(|s| s.members[0].title.contains("#cycle"))
            .unwrap();
        assert!(live.size < top.size || live.worth.score < top.worth.score);
        assert!(top.worth.score > live.worth.score);
    }

    #[test]
    fn an_unstamped_note_takes_the_version_current_when_it_was_written() {
        let e = engine();
        let day = 86_400;
        let t0 = crate::now() - 90 * day;
        let mk = |title: &str, at: i64| NewNode {
            created_at: Some(at),
            version: None,
            body: Some(format!("{title} — body text")),
            title: title.to_string(),
            ..new_like(&add(&e, "seed stranger for the shape"))
        };
        // Written before any set_version, then while 0.2.0 was current.
        let early = e.add_node(mk("#era note before the journal", t0)).unwrap();
        e.set_current_version(Some("0.2.0")).unwrap();
        let clock = e.version_clock().unwrap();
        let later = Node {
            created_at: crate::now() + 1,
            ..early.clone()
        };
        assert_eq!(clock.version_of(&early).as_deref(), Some("<0.2.0"));
        assert_eq!(clock.version_of(&later).as_deref(), Some("0.2.0"));
        let stamped = Node {
            version: Some("v0.1.5".into()),
            ..early
        };
        assert_eq!(clock.version_of(&stamped).as_deref(), Some("0.1.5"));
        let mut vs = vec!["0.10.0", "<0.2.0", "0.2.0"];
        vs.sort_by(|a, b| version_order(a, b));
        assert_eq!(vs, ["<0.2.0", "0.2.0", "0.10.0"]);
    }

    #[test]
    fn review_pages_and_filters() {
        let (e, _, brief) = crowded();
        let second = e
            .review_page(&ReviewQuery {
                offset: 1,
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(second.total, 2);
        assert_eq!(second.subjects.len(), 1);
        assert_eq!(member_ids(&second.subjects[0]), ids(&brief));
        let none = e
            .review_page(&ReviewQuery {
                types: vec![NodeType::Caution],
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(none.total, 0);
    }

    #[test]
    fn a_reviewed_cluster_leaves_the_queue_until_half_its_members_change() {
        let (e, bridge, _) = crowded();
        let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
        assert_eq!(e.mark_reviewed(&all, true).unwrap().len(), 6);
        let page = e.review_page(&query()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.hidden_reviewed, 1);
        let shown = e
            .review_page(&ReviewQuery {
                include_reviewed: true,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(shown.total, 2);
        assert!(
            shown
                .subjects
                .iter()
                .any(|s| s.members.iter().all(|m| m.reviewed))
        );

        // An edit voids that member's mark; one changed note of six keeps
        // the subject hidden, half of them brings it back.
        let rewrite = |n: &Node| {
            e.update_node(
                &n.id,
                NodePatch {
                    body: Some(format!("{} rewritten", n.title)),
                    ..Default::default()
                },
            )
            .unwrap();
        };
        rewrite(&bridge[2]);
        assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 1);
        rewrite(&bridge[3]);
        rewrite(&bridge[4]);
        let page = e.review_page(&query()).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.hidden_reviewed, 0);
        let back = page
            .subjects
            .iter()
            .find(|c| c.members.iter().any(|m| m.id == bridge[2].id))
            .unwrap();
        let fresh = back.members.iter().find(|m| m.id == bridge[2].id).unwrap();
        assert!(!fresh.reviewed);
        assert!(back.members.iter().filter(|m| m.reviewed).count() == 3);
    }

    #[test]
    fn newcomers_reopen_a_reviewed_cluster_once_they_are_half() {
        let (e, bridge, _) = crowded();
        let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
        e.mark_reviewed(&all, true).unwrap();
        add(&e, "#bridge note 9, a fresh restatement");
        assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 1);
        for i in 10..15 {
            add(&e, &format!("#bridge note {i}, a fresh restatement"));
        }
        let page = e.review_page(&query()).unwrap();
        assert_eq!(page.hidden_reviewed, 0);
        assert_eq!(page.total, 2);
    }

    #[test]
    fn a_mark_moves_no_trust_field() {
        let (e, bridge, _) = crowded();
        let before = e.get_node(&bridge[0].id).unwrap().unwrap();
        e.mark_reviewed(std::slice::from_ref(&before.id), true)
            .unwrap();
        let after = e.get_node(&bridge[0].id).unwrap().unwrap();
        assert_eq!(after.confirmed_at, before.confirmed_at);
        assert_eq!(after.approved_at, before.approved_at);
        assert_eq!(after.trust, before.trust);
    }

    #[test]
    fn unmarking_and_unknown_ids() {
        let (e, bridge, _) = crowded();
        let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
        e.mark_reviewed(&all, true).unwrap();
        assert_eq!(
            e.mark_reviewed(&["nope".to_string()], true).unwrap().len(),
            0
        );
        e.mark_reviewed(&all[..1], false).unwrap();
        assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 1);
        e.mark_reviewed(&all[..3], false).unwrap();
        assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 0);
    }

    #[test]
    fn marks_from_a_store_without_them_or_garbled_are_empty() {
        // A graph written by 0.10.4 has no key; a hand-mangled one parses
        // to nothing instead of failing every review.
        let (e, _, _) = crowded();
        assert_eq!(e.review_page(&query()).unwrap().hidden_reviewed, 0);
        e.store.kv_set(MARKS_KEY, "not json").unwrap();
        let page = e.review_page(&query()).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.hidden_reviewed, 0);
    }

    #[test]
    fn archived_notes_lose_their_marks_and_leave_the_index() {
        let (e, bridge, _) = crowded();
        let all: Vec<String> = bridge.iter().map(|n| n.id.clone()).collect();
        e.mark_reviewed(&all, true).unwrap();
        e.merge_nodes(
            &bridge[0].id,
            &all[1..],
            Some("#bridge one consolidated note".into()),
            None,
            Source::Claude,
        )
        .unwrap();
        let index = e.crowding().unwrap();
        assert!(all[1..].iter().all(|id| !index.nodes.contains_key(id)));
        // The next mark prunes the archived victims' entries.
        e.mark_reviewed(&all[..1], true).unwrap();
        let marks = e.review_marks().unwrap();
        assert_eq!(marks.len(), 1);
        assert!(marks.contains_key(&all[0]));
    }

    #[test]
    fn the_index_is_cached_until_the_graph_changes() {
        let (e, _, _) = crowded();
        let a = e.crowding().unwrap();
        let b = e.crowding().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        add(&e, "one more stranger");
        let c = e.crowding().unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        assert_eq!(c.size, a.size + 1);
    }

    #[test]
    fn around_lists_nearest_and_subject_mates() {
        let (e, bridge, _) = crowded();
        let anchor = e
            .add_node(NewNode {
                node_type: NodeType::Anchor,
                title: "crates/engram-mcp/src/bridge.rs".into(),
                ..new_like(&bridge[0])
            })
            .unwrap();
        let tagged = e
            .add_node(NewNode {
                tags: vec!["process-model".into()],
                ..new_like(&bridge[0])
            })
            .unwrap();
        let far = add(&e, "a stranger that is about the bridge file");
        for from in [&bridge[0].id, &far.id] {
            e.add_edge(NewEdge {
                edge_type: EdgeType::About,
                from_id: from.clone(),
                to_id: anchor.id.clone(),
                source: Source::Claude,
                note: None,
                confidence: None,
                strength: None,
                status: None,
            })
            .unwrap();
        }
        e.update_node(
            &bridge[0].id,
            NodePatch {
                tags: Some(vec!["process-model".into()]),
                ..Default::default()
            },
        )
        .unwrap();

        let view = e.around(&bridge[0].id).unwrap();
        assert_eq!(view.node.id, bridge[0].id);
        // The five other #bridge notes and the tagged sibling (also #bridge)
        // tie for nearest; ties fall in id order, so read all six.
        let near: Vec<&str> = view.nearest.iter().take(6).map(|n| n.id.as_str()).collect();
        for b in bridge[1..].iter().chain([&tagged]) {
            assert!(near.contains(&b.id.as_str()), "{near:?}");
        }
        assert!(view.nearest[..6].iter().all(|n| n.close));
        assert!(view.nearest[0].close);
        let mate = |id: &str| view.subject.iter().find(|n| n.id == id);
        assert_eq!(
            mate(&far.id).unwrap().via,
            ["anchor:crates/engram-mcp/src/bridge.rs"]
        );
        assert_eq!(mate(&tagged.id).unwrap().via, ["tag:process-model"]);
        assert!(
            view.node
                .edges
                .iter()
                .any(|e| e.verb == "about" && e.dir == "out")
        );

        // An anchor's view lists what is about it.
        let view = e.around(&anchor.id).unwrap();
        assert!(view.nearest.is_empty(), "anchors are not in the index");
        let about: Vec<&str> = view.subject.iter().map(|n| n.id.as_str()).collect();
        assert!(about.contains(&far.id.as_str()) && about.contains(&bridge[0].id.as_str()));

        assert!(e.around("missing").is_err());
    }

    fn new_like(n: &Node) -> NewNode {
        NewNode {
            node_type: n.node_type.clone(),
            title: format!("{} (sibling)", n.title),
            body: None,
            created_at: None,
            durability: Durability::Stable,
            source: Source::Claude,
            session_id: None,
            status: None,
            code_refs: vec![],
            tags: vec![],
            version: None,
            props: None,
            fields: None,
        }
    }

    #[test]
    fn islands_are_notes_no_edge_and_no_tag_reaches() {
        let e = engine();
        let lone = add(&e, "#delivery the floor is calibrated per graph");
        let near = add(&e, "#delivery the floor moved to phantom probes");
        let tagged = add(&e, "#bridge a tagged note");
        e.update_node(
            &tagged.id,
            NodePatch {
                tags: Some(vec!["bridge".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        let a = add(&e, "#brief linked one");
        let b = add(&e, "#brief linked two");
        e.add_edge(NewEdge {
            from_id: a.id.clone(),
            to_id: b.id.clone(),
            edge_type: EdgeType::BuildsOn,
            source: Source::Claude,
            confidence: None,
            note: None,
            strength: None,
            status: None,
        })
        .unwrap();
        let page = e.islands_page(false, 0, 10).unwrap();
        let ids: Vec<&str> = page.islands.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(page.total, 2, "{ids:?}");
        assert!(ids.contains(&lone.id.as_str()) && ids.contains(&near.id.as_str()));
        // Each points at the other: the link a sleep would add.
        let first = &page.islands[0];
        assert!(first.nearest[0].id == lone.id || first.nearest[0].id == near.id);
        assert!(first.nearest[0].close);

        e.mark_reviewed(std::slice::from_ref(&lone.id), true)
            .unwrap();
        let page = e.islands_page(false, 0, 10).unwrap();
        assert_eq!((page.total, page.hidden_reviewed), (1, 1));
        assert_eq!(e.islands_page(true, 0, 10).unwrap().total, 2);
    }

    #[test]
    fn the_conflicts_view_lists_standing_edges_with_the_newer_side() {
        let e = engine();
        let old = add(&e, "#delivery the floor is 0.22");
        let new = add(&e, "#delivery the floor is calibrated");
        e.add_edge(NewEdge {
            from_id: new.id.clone(),
            to_id: old.id.clone(),
            edge_type: EdgeType::ConflictsWith,
            source: Source::Claude,
            confidence: None,
            note: Some("measured".into()),
            strength: None,
            status: None,
        })
        .unwrap();
        let view = e.conflicts_view().unwrap();
        assert_eq!(view.standing.len(), 1);
        let c = &view.standing[0];
        assert_eq!(
            (c.a.id.as_str(), c.b.id.as_str()),
            (new.id.as_str(), old.id.as_str())
        );
        assert_eq!(c.newer, "a");
        assert_eq!(c.note.as_deref(), Some("measured"));
    }

    #[test]
    fn a_pile_below_the_minimum_is_no_subject() {
        let e = engine();
        for i in 0..4 {
            add(&e, &format!("#bridge note {i} on reconnects"));
        }
        for i in 0..30 {
            add(&e, &format!("stranger {i} about something else entirely"));
        }
        assert_eq!(
            e.review_page(&query()).unwrap().total,
            0,
            "a pile of 4 is a pocket"
        );
    }

    #[test]
    fn the_report_carries_heat_and_open_subjects() {
        let (e, bridge, _) = crowded();
        let report = e.crowding_report().unwrap();
        assert_eq!(report.size, 41);
        assert_eq!(report.min_size, 5);
        assert_eq!(report.subjects.len(), 2);
        assert_eq!(report.subjects[0].members.len(), 6);
        assert!(
            report.subjects[0]
                .name
                .as_deref()
                .is_some_and(|n| n.contains("bridge"))
        );
        assert_eq!(report.nodes[&bridge[0].id].crowding, 5);
        assert_eq!(report.nodes[&bridge[0].id].heat, 1.0);
        assert!(serde_json::to_value(&report).unwrap()["nodes"].is_object());
    }
}
