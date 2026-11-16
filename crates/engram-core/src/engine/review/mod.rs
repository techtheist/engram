//! Sleep's reading side: the crowding index over this graph, the
//! review queue of subjects it cuts (see [`crate::crowding`]), ranked by
//! how much they are worth consolidating, one note's neighbourhood, the
//! conflicts to settle (standing `conflicts-with` edges beside pending
//! suspects), the islands (notes no edge and no tag reaches), and reviewed
//! marks.
//! Nominations only — nothing here writes a node, an edge, or trust; the
//! one write is the reviewed mark, which lives in the store's kv beside the
//! graph (older binaries never read the key, so it costs them nothing).

mod subjects;
#[cfg(test)]
mod tests;
mod views;

use crate::sync::MutexExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;

use super::{Engine, is_anchor, is_tombstone};
use crate::crowding::{CrowdIndex, fnv};
use crate::{FoundBridge, Node, NodeType, Result, SuspectView};

/// The kv key holding reviewed marks: a JSON object {node id: content
/// fingerprint in hex}.
pub(super) const MARKS_KEY: &str = "review.marks";
/// Body characters a review member carries; `get_node` reads the rest.
pub(super) const SNIPPET: usize = 240;
/// Edges listed per member / per `around` view.
pub(super) const MEMBER_EDGES: usize = 4;
pub(super) const AROUND_EDGES: usize = 20;
/// Neighbours and subject-mates an `around` view lists.
pub(super) const AROUND_NEAR: usize = 12;
/// Members a subject shows in full; the rest come as id + title lines.
pub(super) const SHOWN: usize = 12;
/// Closest member pairs a subject lists — the merge candidates.
pub(super) const PAIRS: usize = 12;
/// A member younger than this is live work, not history to fold.
pub(super) const SETTLED_DAYS: i64 = 14;
/// Nearest notes an island lists.
pub(super) const ISLAND_NEAR: usize = 3;
/// Characters of a linked note's title a member's edge carries.
pub(super) const EDGE_TITLE: usize = 80;
/// Found bridges an island lists, and a subject lists leaving it:
/// notes a bridge note ties in while no edge does — where a link may go.
pub(super) const ISLAND_BRIDGES: usize = 2;
pub(super) const SUBJECT_BRIDGES: usize = 3;

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

pub(super) fn is_zero(n: &usize) -> bool {
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
    /// size × (1 + ln versions) ×, one decimal.
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
    /// Notes outside the subject that a bridge note ties to a member while
    /// no edge does: `from` is the member.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<SubjectBridge>,
}

/// A found bridge leaving a subject: which member it starts from.
#[derive(Debug, Clone, Serialize)]
pub struct SubjectBridge {
    pub from: String,
    #[serde(flatten)]
    pub found: FoundBridge,
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
    /// Notes a bridge note ties it to — a tie the text states.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<FoundBridge>,
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
pub(super) fn content_fp(n: &Node) -> u64 {
    fnv(&[
        n.node_type.as_str().as_bytes(),
        n.title.as_bytes(),
        n.body.as_deref().unwrap_or("").as_bytes(),
    ])
}

/// [`super::cosine`] on the review's `f32` scale.
pub(super) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    super::cosine(a, b) as f32
}

pub(super) fn snippet(body: Option<&str>) -> String {
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
            .locked()
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
        *self.crowd_cache.locked() = Some(index.clone());
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
}
