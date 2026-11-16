//! The other review views: one note's neighbourhood, the crowding report,
//! standing conflicts and islands.

use super::*;

impl Engine {
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
    pub(super) fn island_notes(&self, include_reviewed: bool) -> Result<(Vec<Node>, usize)> {
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
        let paged: Vec<Node> = notes.into_iter().skip(offset).take(limit).collect();
        let ids: Vec<String> = paged.iter().map(|n| n.id.clone()).collect();
        let found = self.found_bridges_many(&ids, ISLAND_BRIDGES)?;
        for (n, bridges) in paged.into_iter().zip(found) {
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
                bridges,
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
