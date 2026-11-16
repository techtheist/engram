//! Graph reads (neighbours, traversal, worklists, timelines) and import/export.

use super::*;

impl Engine {
    pub fn edges_out(&self, id: &str) -> Result<Vec<Edge>> {
        self.store.edges_out(id)
    }

    pub fn edges_in(&self, id: &str) -> Result<Vec<Edge>> {
        self.store.edges_in(id)
    }

    pub fn list_open(&self, types: &[NodeType]) -> Result<Vec<Node>> {
        self.store.list_open(types)
    }

    /// The worklist: open Problems/Intents, plus (when `include_conflicts`)
    /// nodes sitting on an active `conflicts-with` edge — deduped by id.
    pub fn worklist(&self, types: &[NodeType], include_conflicts: bool) -> Result<Vec<Node>> {
        let mut nodes = self.store.list_open(types)?;
        if include_conflicts {
            let seen: std::collections::HashSet<String> =
                nodes.iter().map(|n| n.id.clone()).collect();
            for n in self.store.nodes_in_active_conflicts()? {
                if !seen.contains(&n.id) {
                    nodes.push(n);
                }
            }
        }
        Ok(nodes)
    }

    pub fn traverse(
        &self,
        from: &str,
        edge_types: &[EdgeType],
        depth: usize,
    ) -> Result<(Vec<Node>, Vec<Edge>)> {
        self.store.traverse(from, edge_types, depth)
    }

    /// The whole graph, for the pane's full-graph render.
    pub fn graph(&self) -> Result<(Vec<Node>, Vec<Edge>)> {
        Ok((self.store.all_nodes()?, self.store.all_edges()?))
    }

    /// Export the whole graph as a portable, diffable snapshot. Nodes and edges
    /// are sorted (created_at, id), and the computed trust fields are zeroed —
    /// they're a function of "now", and a time-dependent export would never
    /// produce stable git diffs. Importers recompute trust from the timestamps.
    pub fn export(&self) -> Result<ExportGraph> {
        let mut nodes = self.store.all_nodes()?;
        let mut edges = self.store.all_edges()?;
        let key_n = |n: &Node| (n.created_at, n.id.clone());
        let key_e = |e: &Edge| (e.created_at, e.id.clone());
        nodes.sort_by_key(key_n);
        edges.sort_by_key(key_e);
        for n in &mut nodes {
            n.trust = 0.0;
            n.stale = false;
        }
        Ok(ExportGraph {
            version: EXPORT_VERSION,
            nodes,
            edges,
            // Exports embed their ontology: a customized graph's
            // dump must re-import as the same graph. Uncustomized stays bare
            // — an old dump and a new default dump mean the same thing.
            config: self.stored_graph_config(),
            tasks: {
                let mut tasks = self.store.all_tasks()?;
                tasks.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
                tasks
            },
        })
    }

    /// Import a snapshot: upsert nodes+edges by id in one transaction, then
    /// regenerate embeddings. Idempotent — re-importing the same graph is a
    /// no-op beyond refreshing fields. Unknown future versions are rejected.
    pub fn import(&self, graph: ExportGraph) -> Result<ImportSummary> {
        if graph.version > EXPORT_VERSION {
            return Err(crate::Error::Parse {
                kind: "export version",
                value: graph.version.to_string(),
            });
        }
        // Pre-trust-v2 exports carry last_seen but no confirmed_at; restore
        // the same backfill the schema migration applies, or every imported
        // node's trust anchor collapses to created_at and a healthy backup
        // comes back stale (and decay-eligible).
        let mut nodes = graph.nodes;
        for n in &mut nodes {
            n.confirmed_at = n.confirmed_at.or(n.last_seen);
        }
        let graph = ExportGraph { nodes, ..graph };
        self.store.import_raw(&graph.nodes, &graph.edges)?;
        for n in &graph.nodes {
            self.embed_node(n)?;
        }
        // A dump that embeds its ontology restores it — validated like any
        // config write; an invalid one fails the whole import loudly rather
        // than silently restoring a graph whose types have no definitions.
        if let Some(cfg) = &graph.config {
            self.set_graph_config(cfg)?;
        }
        for t in &graph.tasks {
            self.store.upsert_task(t)?;
        }
        let (nodes, edges) = (graph.nodes.len(), graph.edges.len());
        // One summary row: per-entity rows for a bulk restore would drown the
        // journal, and the snapshot file itself is the before/after record.
        self.audit(
            "imported",
            "graph",
            "",
            Some(format!("{nodes} nodes / {edges} edges")),
            None,
            Some(serde_json::json!({ "nodes": nodes, "edges": edges })),
            None,
        )?;
        Ok(ImportSummary { nodes, edges })
    }

    /// Every generation linked to `id` through `replaces`, oldest first; each
    /// superseded one carries the note of the edge that retired it.
    pub fn timeline(&self, id: &str) -> Result<Vec<TimelineEntry>> {
        let cfg = self.store.config();
        let supersession = cfg.supersession_verb();
        let Some(start) = self.store.get_node(id)? else {
            return Err(crate::Error::NotFound(format!("node {id}")));
        };
        let mut seen = std::collections::HashSet::from([start.id.clone()]);
        let mut queue = vec![start.id.clone()];
        let mut nodes = vec![start];
        let mut replaced_note = std::collections::HashMap::new();
        // (newer, older) pairs — the chain's own topology orders generations;
        // created_at only breaks ties (same-second writes sort randomly).
        let mut pairs = std::collections::HashSet::new();
        while let Some(cur) = queue.pop() {
            let mut edges = self.store.edges_out(&cur)?;
            edges.extend(self.store.edges_in(&cur)?);
            for e in edges {
                if e.edge_type.as_str() != supersession {
                    continue;
                }
                // The edge reads "from replaces to": its note explains why
                // the `to` generation was retired.
                replaced_note.insert(e.to_id.clone(), e.note);
                pairs.insert((e.from_id.clone(), e.to_id.clone()));
                for next in [e.from_id, e.to_id] {
                    if seen.insert(next.clone())
                        && let Some(n) = self.store.get_node(&next)?
                    {
                        nodes.push(n);
                        queue.push(next);
                    }
                }
            }
        }
        // Generation = longest replaces-path down to an original (0). Sorting
        // by it puts every node after everything it (transitively) replaced.
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let mut adj: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        for (newer, older) in &pairs {
            adj.entry(newer.as_str()).or_default().push(older.as_str());
        }
        let mut memo = std::collections::HashMap::new();
        for id in &ids {
            generation(id.as_str(), &adj, &mut memo);
        }
        nodes.sort_by(|a, b| {
            memo[a.id.as_str()]
                .cmp(&memo[b.id.as_str()])
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
        Ok(nodes
            .into_iter()
            .map(|n| TimelineEntry {
                replaced_note: replaced_note.get(&n.id).cloned().flatten(),
                id: n.id,
                node_type: n.node_type,
                title: n.title,
                created_at: n.created_at,
                valid_until: n.valid_until,
            })
            .collect())
    }

    /// Tags in use, freshest first (the pane's dropdown; the brief's vocabulary).
    pub fn tags(&self, limit: usize) -> Result<Vec<TagStat>> {
        self.store.tag_stats(limit)
    }
}
