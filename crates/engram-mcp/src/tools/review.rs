//! Review tools: the suspect queue and node listings.

use crate::*;

#[tool_router(router = review_router, vis = "pub(crate)")]
impl Engram {
    #[tool(
        description = "Pending suspected conflicts: unlinked look-alike pairs \
        awaiting judgment (nli_label is a triage hint, not a verdict). Judge \
        each with resolve_suspect."
    )]
    pub(crate) async fn list_suspects(
        &self,
        Parameters(a): Parameters<ProjectArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let suspects = self.mcp(&engine).suspects().map_err(map_err)?;
        self.reply(&json!({ "suspects": suspects }))
    }

    #[tool(description = "Judge a suspect: `conflict` links conflicts-with, \
        `replaces` links AND archives the older node, `dismiss` = fine \
        together (never re-raised).")]
    pub(crate) async fn resolve_suspect(
        &self,
        Parameters(a): Parameters<ResolveSuspectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let verdict = SuspectVerdict::parse(&a.verdict).map_err(map_err)?;
        let engine = self.write_engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .resolve_suspect(&a.id, verdict, Source::Agent)
            .map_err(map_err)?;
        if matches!(verdict, SuspectVerdict::Conflict) {
            // The judge doesn't need its own verdict pushed back at it.
            let _ = self.drain_conflict_alerts();
        }
        self.reply_write(json!({ "ok": true, "edge": edge }), &a.project)
    }

    #[tool(description = "Lossless paged read: complete nodes with filters \
        (types, status, tag, pinned, include_archived) — the bulk read for \
        reviews and exports where snippets won't do. Newest first; page with \
        limit/offset; sort: recent | most-connected | least-connected | \
        crowded. `around: <id>` = that note's nearest notes and the notes \
        sharing its anchors/tags/refs, no search cut-off. Sleep queues: \
        `view: \"conflicts\"` = standing conflicts-with edges + pending \
        suspects; `\"review\"` = named subjects (piles of close notes), most \
        worth consolidating first, central members in full, the closest \
        pairs; `\"islands\"` = notes no edge and no tag reaches, with their \
        nearest notes. Reviewed ones hidden.")]
    pub(crate) async fn list_nodes(
        &self,
        Parameters(a): Parameters<ListNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let status = a
            .status
            .map(|s| NodeStatus::parse(&s))
            .transpose()
            .map_err(map_err)?;
        let tag = a
            .tag
            .map(|t| engram_core::normalize_tags(&[t]))
            .and_then(|mut v| v.pop());
        let engine = self.engine_for(&a.project)?;
        if let Some(id) = &a.around {
            let view = self.mcp(&engine).around(id).map_err(map_err)?;
            return self.reply(&view);
        }
        match a.view.as_deref() {
            None | Some("nodes") => {}
            Some("review") => {
                let q = engram_core::ReviewQuery {
                    types,
                    tag,
                    include_reviewed: a.include_reviewed.unwrap_or(false),
                    offset: a.offset.unwrap_or(0),
                    limit: a.limit.unwrap_or(1).min(10),
                };
                let page = self.mcp(&engine).review_page(&q).map_err(map_err)?;
                return self.reply(&page);
            }
            Some("conflicts") => {
                let view = self.mcp(&engine).conflicts_view().map_err(map_err)?;
                return self.reply(&view);
            }
            Some("islands") => {
                let page = self
                    .mcp(&engine)
                    .islands_page(
                        a.include_reviewed.unwrap_or(false),
                        a.offset.unwrap_or(0),
                        a.limit.unwrap_or(20).min(100),
                    )
                    .map_err(map_err)?;
                return self.reply(&page);
            }
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("view {other:?}: use nodes | conflicts | review | islands"),
                    None,
                ));
            }
        }
        let (mut nodes, _) = self.mcp(&engine).graph().map_err(map_err)?;
        nodes.retain(|n| {
            (a.include_archived.unwrap_or(false) || n.valid_until.is_none())
                && (types.is_empty() || types.contains(&n.node_type))
                && status.is_none_or(|s| n.status == Some(s))
                && tag.as_ref().is_none_or(|t| n.tags.contains(t))
                && a.pinned.is_none_or(|p| n.trust_override.is_some() == p)
        });
        match a.sort.as_deref() {
            None | Some("recent") => {
                // Ids are time-sortable, so this is newest-first creation order.
                nodes.sort_by(|x, y| y.id.cmp(&x.id));
            }
            Some(order @ ("most-connected" | "least-connected")) => {
                // Degree over live edges: the least-connected end surfaces
                // reachability islands (no links means text search is the
                // only road in), the most-connected end surfaces the hubs.
                let mut degree: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for e in self.mcp(&engine).store().all_edges().map_err(map_err)? {
                    *degree.entry(e.from_id).or_default() += 1;
                    *degree.entry(e.to_id).or_default() += 1;
                }
                nodes.sort_by(|x, y| {
                    let (dx, dy) = (
                        degree.get(&x.id).copied().unwrap_or(0),
                        degree.get(&y.id).copied().unwrap_or(0),
                    );
                    match order {
                        "most-connected" => dy.cmp(&dx).then(y.id.cmp(&x.id)),
                        _ => dx.cmp(&dy).then(y.id.cmp(&x.id)),
                    }
                });
            }
            Some("crowded") => {
                let index = self.mcp(&engine).crowding().map_err(map_err)?;
                let total = nodes.len();
                let offset = a.offset.unwrap_or(0);
                let limit = a.limit.unwrap_or(30).min(200);
                let crowding = |id: &str| index.nodes.get(id).map_or(0, |c| c.crowding);
                nodes.sort_by(|x, y| crowding(&y.id).cmp(&crowding(&x.id)).then(y.id.cmp(&x.id)));
                let page: Vec<serde_json::Value> = nodes
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .map(|n| {
                        let c = crowding(&n.id);
                        let mut v = json!(n);
                        v["crowding"] = json!(c);
                        v
                    })
                    .collect();
                return self.reply(&json!({
                    "total": total,
                    "offset": offset,
                    "band": index.band,
                    "nodes": page,
                }));
            }
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!(
                        "sort {other:?}: use recent | most-connected | least-connected | crowded"
                    ),
                    None,
                ));
            }
        }
        let total = nodes.len();
        let offset = a.offset.unwrap_or(0);
        let limit = a.limit.unwrap_or(30).min(200);
        let page: Vec<Node> = nodes.into_iter().skip(offset).take(limit).collect();
        self.reply(&json!({ "total": total, "offset": offset, "nodes": page }))
    }

    #[tool(description = "List the live worklist: open Problems and Intents.")]
    pub(crate) async fn list_open(
        &self,
        Parameters(a): Parameters<ListOpenArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let engine = self.engine_for(&a.project)?;
        let nodes = self
            .mcp(&engine)
            .worklist(&types, a.include_conflicts.unwrap_or(true))
            .map_err(map_err)?;
        self.reply(&nodes)
    }
}
