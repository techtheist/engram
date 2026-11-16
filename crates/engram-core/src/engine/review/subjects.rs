//! The review queue: subjects ranked by how much consolidating them is worth.

use super::*;

/// "v0.7.0" and "0.7.0" are one version.
pub(super) fn bare(v: &str) -> &str {
    v.trim_start_matches(['v', 'V'])
}

/// When each working version began, from the `set_version` journal: a note
/// captured before version stamping (or with it off) gets the version that
/// was current when it was written. Notes older than the journal share one
/// label, "<first", so a subject's history before it still counts once.
pub(super) struct VersionClock(Vec<(i64, String)>);

impl VersionClock {
    pub(super) fn version_of(&self, n: &Node) -> Option<String> {
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
pub(super) fn version_order(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |v: &str| (!v.starts_with('<'), v.trim_start_matches('<').to_string());
    let (ka, kb) = (key(a), key(b));
    ka.0.cmp(&kb.0)
        .then_with(|| crate::tasks::version_cmp(&ka.1, &kb.1).unwrap_or_else(|| ka.1.cmp(&kb.1)))
}

impl Engine {
    pub(super) fn review_member(
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
    pub(super) fn link_between(&self, a: &str, b: &str) -> Result<Option<String>> {
        Ok(self
            .store
            .edges_out(a)?
            .into_iter()
            .chain(self.store.edges_in(a)?)
            .find(|e| e.valid_until.is_none() && (e.to_id == b || e.from_id == b))
            .map(|e| e.edge_type.as_str().to_string()))
    }

    /// Anchors a note is `about` (any out-edge into an anchor-role node).
    pub(super) fn anchors_of(&self, id: &str) -> Result<Vec<(String, String)>> {
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
    pub(super) fn version_clock(&self) -> Result<VersionClock> {
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
    pub(super) fn worth(
        &self,
        members: &[&Node],
        working: Option<&str>,
        clock: &VersionClock,
    ) -> Worth {
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
    pub(super) fn review_subjects(
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
            let shown: Vec<String> = all.iter().take(SHOWN).map(|n| n.id.clone()).collect();
            let mut bridges: Vec<SubjectBridge> = Vec::new();
            for (from, found) in shown
                .iter()
                .zip(self.found_bridges_many(&shown, SUBJECT_BRIDGES)?)
            {
                for f in found {
                    if bridges.len() < SUBJECT_BRIDGES
                        && !ids.contains(f.note.id.as_str())
                        && !bridges.iter().any(|b| b.found.note.id == f.note.id)
                    {
                        bridges.push(SubjectBridge {
                            from: from.clone(),
                            found: f,
                        });
                    }
                }
            }
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
                bridges,
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
}
