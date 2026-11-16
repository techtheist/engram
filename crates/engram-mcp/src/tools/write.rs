//! Write tools: notes, links, versions, approvals, merges.

use crate::*;

#[tool_router(router = write_router, vis = "pub(crate)")]
impl Engram {
    #[tool(description = "Create a memory node: {type, title} required (types: \
        Decision | Principle | Caution | Problem | Resolution | Insight | \
        Intent | Anchor | Tombstone), body/tags optional; edges go through `link`, never \
        inline. The response is a verdict, not a receipt — act on it: \
        {matched, created: false} = near-duplicate, merge via update_node \
        (nli_label=contradiction = a NEGATED duplicate — read it first, \
        likely conflicts-with instead); `warnings` = landed near \
        contradicted/superseded/tombstoned canon (tombstoned = deliberately \
        removed — don't re-add unless the user says so); `missing_code_refs` \
        = fix or drop; `suspects` = judge each with resolve_suspect now, \
        telling the user about genuine contradictions.")]
    pub(crate) async fn add_note(
        &self,
        Parameters(a): Parameters<AddNoteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = a.project.clone();
        let payload = self.create_note(a)?;
        self.reply_write(payload, &project)
    }

    /// The add_note core, shared with the batch form. Each note resolves its
    /// own `project` — a write addressed to `all` is refused by the hub with
    /// the home-graph pointer.
    pub(crate) fn create_note(&self, a: AddNoteArgs) -> Result<serde_json::Value, ErrorData> {
        let node_type = NodeType::parse(&a.node_type).map_err(map_err)?;
        let engine = self.write_engine_for(&a.project)?;
        let explicit = a.project.clone();
        // Durability defaults from the ontology; born-open status for
        // worklist types is applied by Engine::add_node (the write boundary
        // owns it, so every surface gets it).
        let durability = match a.durability {
            Some(d) => Durability::parse(&d).map_err(map_err)?,
            None => engine.locked().default_durability(&node_type),
        };
        let created_at = match a.created_at.as_deref() {
            None => None,
            Some(raw) => Some(engram_core::parse_day(raw).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("created_at {raw:?} is not YYYY-MM-DD or unix seconds"),
                    None,
                )
            })?),
        };
        // Judged with the lock released: a heavy logic layer must
        // not park every other request on the graph behind this write.
        let outcome = Engine::add_node_checked_shared(
            &engine,
            self.writer(),
            NewNode {
                node_type,
                title: a.title,
                body: a.body,
                created_at,
                durability,
                source: Source::Agent,
                session_id: a.session_id.or_else(|| Some(self.session_id.to_string())),
                status: None,
                code_refs: a.code_refs,
                tags: a.tags,
                version: a.version,
                props: None,
                fields: a.fields,
            },
        )
        .map_err(map_err)?;
        Ok(match outcome {
            WriteOutcome::Created {
                node,
                warnings,
                suspects,
                missing_refs,
                canon,
            } => {
                // born-in provenance: a live MCP write was born in
                // whatever exchange is happening right now — park it; the
                // harvester links it once the transcript catches up.
                engine.locked().park_provenance(&node.id, node.created_at);
                let mut out = json!({ "id": node.id, "created": true });
                if !warnings.is_empty() {
                    out["warnings"] = json!(warnings);
                }
                if !missing_refs.is_empty() {
                    out["missing_code_refs"] = json!(missing_refs);
                    out["refs_note"] = json!(
                        "these code_refs don't resolve in the repo right now — fix the paths or drop them"
                    );
                }
                if !suspects.is_empty() {
                    out["suspects"] = json!(suspects);
                    out["action_required"] = json!(SUSPECT_ACTION);
                }
                Self::attach_canon(&mut out, &canon);
                if explicit.is_some() {
                    // A batch item aimed at another graph says so; the
                    // envelope names the session's own project.
                    out["project"] = json!(self.project_label(&explicit));
                }
                out
            }
            WriteOutcome::Matched {
                node,
                similarity,
                nli_label,
                nli_score,
            } => {
                let mut out = json!({
                    "matched": node.id,
                    "created": false,
                    "title": node.title,
                    "similarity": similarity,
                });
                if let (Some(label), Some(score)) = (&nli_label, nli_score) {
                    out["nli_label"] = json!(label);
                    out["nli_score"] = json!(score);
                    if label == "contradiction" {
                        out["action_required"] = json!(
                            "The near-duplicate may CONTRADICT your text (negated duplicate — \
                             'use X' vs 'don't use X'). Read the matched node before merging; \
                             if it genuinely disagrees, capture yours as a new node and link \
                             conflicts-with instead of updating the match."
                        );
                    }
                }
                out
            }
        })
    }

    #[tool(description = "Batch add_note: `notes` is an array of add_note \
        items (each: required type + title, optional body/tags/…; NO links \
        inside items — `link` the returned ids afterwards). Same per-item \
        checks and verdicts, positional results ({id, created} | {matched, \
        created: false} | {ok: false, error}); one bad item never blocks the \
        rest.")]
    pub(crate) async fn add_notes(
        &self,
        Parameters(a): Parameters<AddNotesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.notes.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} notes per call"),
                None,
            ));
        }
        let results: Vec<serde_json::Value> = a
            .notes
            .into_iter()
            .map(|item| {
                self.create_note(item)
                    .unwrap_or_else(|e| json!({ "ok": false, "error": e.message }))
            })
            .collect();
        self.reply_write(json!({ "results": results }), &None)
    }

    #[tool(description = "Set (or clear, with null) the graph's current working \
        version, stamped on new notes. Call when the project moves on (release \
        cut, cycle open). Setting enables version tracking; clearing never \
        toggles it.")]
    pub(crate) async fn set_version(
        &self,
        Parameters(a): Parameters<SetVersionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.write_engine_for(&a.project)?;
        let (previous, history, enabled_by_this_call, settled) = {
            let engine = self.mcp(&engine);
            let was_on = engine.config().versioning.enabled;
            let started = engram_core::now();
            let previous = engine
                .set_current_version(a.version.as_deref())
                .map_err(map_err)?;
            // Version rules on tasks fired with the switch — say which.
            let settled: Vec<String> = engine
                .tasks()
                .map_err(map_err)?
                .into_iter()
                .filter(|t| t.updated_at >= started)
                .filter_map(|t| t.auto.map(|why| format!("{} — {why}", t.title)))
                .collect();
            let history: Vec<String> = engine
                .audit_log(None, Some("version"), 10)
                .map_err(map_err)?
                .entries
                .into_iter()
                .filter_map(|r| r.title)
                .collect();
            (previous, history, a.version.is_some() && !was_on, settled)
        };
        let mut reply = json!({
            "ok": true,
            "previous": previous,
            "current": a.version,
            "history": history,
        });
        if !settled.is_empty() {
            reply["tasks"] = json!(settled);
        }
        if enabled_by_this_call {
            reply["versioning"] = json!(
                "was off — enabled by this call; new version-bound notes are stamped from now on"
            );
        }
        self.reply_write(reply, &a.project)
    }

    #[tool(description = "Delete one edge by id (repair a mislink). Nodes are \
        never deleted here — hard delete is user-only.")]
    pub(crate) async fn unlink(
        &self,
        Parameters(a): Parameters<IdArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.write_engine_for(&a.project)?;
        let removed = self.mcp(&engine).delete_edge(&a.id).map_err(map_err)?;
        if !removed {
            return Err(ErrorData::invalid_params(
                format!("edge not found: {}", a.id),
                None,
            ));
        }
        self.reply_write(json!({ "ok": true }), &a.project)
    }

    #[tool(
        description = "Update an edge's status (active | resolved | dismissed), \
        note, or confidence."
    )]
    pub(crate) async fn update_edge(
        &self,
        Parameters(a): Parameters<UpdateEdgeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let patch = EdgePatch {
            // Retype stays a pane action; Claude repairs a wrong verb with
            // unlink + link (which re-states the sentence deliberately).
            edge_type: None,
            status: a
                .status
                .map(|s| EdgeStatus::parse(&s))
                .transpose()
                .map_err(map_err)?,
            note: a.note,
            confidence: a.confidence,
            strength: None,
        };
        let engine = self.write_engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .update_edge(&a.id, patch)
            .map_err(map_err)?;
        self.reply_write(json!({ "ok": true, "id": edge.id }), &a.project)
    }

    #[tool(description = "Link two nodes with a sentence-shaped edge: \
        {from, to, type} — `type` is the verb, one of about | because | \
        answers | builds-on | replaces | conflicts-with | needs, and \
        \"from <verb> to\" must read as an English sentence.")]
    pub(crate) async fn link(
        &self,
        Parameters(a): Parameters<LinkArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let edge_type = EdgeType::parse(&a.edge_type).map_err(map_err)?;
        let engine = self.write_engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .add_edge(NewEdge {
                edge_type,
                from_id: a.from,
                to_id: a.to,
                source: Source::Agent,
                note: a.note,
                confidence: a.confidence,
                strength: None,
                status: None,
            })
            .map_err(map_err)?;
        let contradiction =
            engine.locked().config().contradiction_verb() == edge.edge_type.as_str();
        if contradiction {
            // This session just recorded the contradiction deliberately — it
            // already knows; don't echo its own alert back on the next call.
            let _ = self.drain_conflict_alerts();
        }
        self.reply_write(json!({ "id": edge.id }), &a.project)
    }

    #[tool(description = "Approve a node: trust restarts at 100%. ONLY on \
        explicit user demand or after word-by-word verification against current \
        reality; routine still-true confirmations belong in update_node.")]
    pub(crate) async fn approve_node(
        &self,
        Parameters(a): Parameters<ApproveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.write_engine_for(&a.project)?;
        let node = self.mcp(&engine).approve(&a.id).map_err(map_err)?;
        self.reply_write(
            json!({ "ok": true, "id": node.id, "trust": node.trust }),
            &a.project,
        )
    }

    #[tool(
        description = "Patch a node (merge / reclassify / confirm still true — \
        a deliberate update stamps confirmed_at, which is how a \
        verified-still-true stale node gets its trust back). The response is \
        an add_note-style verdict with the same act-now duties."
    )]
    pub(crate) async fn update_node(
        &self,
        Parameters(a): Parameters<UpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let project = a.project.clone();
        let payload = self.patch_node(a)?;
        self.reply_write(payload, &project)
    }

    /// The update_node core, shared with the batch form.
    pub(crate) fn patch_node(&self, a: UpdateArgs) -> Result<serde_json::Value, ErrorData> {
        let reviewed = a.reviewed;
        let content = a.node_type.is_some()
            || a.title.is_some()
            || a.body.is_some()
            || a.durability.is_some()
            || a.status.is_some()
            || a.code_refs.is_some()
            || a.tags.is_some()
            || a.version.is_some()
            || a.fields.is_some();
        if let (Some(mark), false) = (reviewed, content) {
            // A bare mark is not an update: no confirmed_at, no verdict.
            let engine = self.write_engine_for(&a.project)?;
            let done = self
                .mcp(&engine)
                .mark_reviewed(std::slice::from_ref(&a.id), mark)
                .map_err(map_err)?;
            if done.is_empty() {
                return Err(ErrorData::invalid_params(
                    format!("node {} not found", a.id),
                    None,
                ));
            }
            return Ok(json!({ "ok": true, "id": a.id, "reviewed": mark }));
        }
        let patch = NodePatch {
            version: a.version,
            node_type: a
                .node_type
                .map(|t| NodeType::parse(&t))
                .transpose()
                .map_err(map_err)?,
            title: a.title,
            body: a.body,
            durability: a
                .durability
                .map(|d| Durability::parse(&d))
                .transpose()
                .map_err(map_err)?,
            status: a
                .status
                .map(|s| NodeStatus::parse(&s))
                .transpose()
                .map_err(map_err)?,
            valid_until: None,
            code_refs: a.code_refs,
            tags: a.tags,
            fields: a.fields,
        };
        let engine = self.write_engine_for(&a.project)?;
        let engram_core::CheckedUpdate {
            node,
            warnings,
            suspects,
            missing_refs,
            canon,
        } = Engine::update_node_checked_shared(&engine, self.writer(), &a.id, patch)
            .map_err(map_err)?;
        let mut out = json!({ "ok": true, "id": node.id });
        if let Some(mark) = reviewed {
            // After the edit, so the mark vouches for the new text.
            self.mcp(&engine)
                .mark_reviewed(std::slice::from_ref(&node.id), mark)
                .map_err(map_err)?;
            out["reviewed"] = json!(mark);
        }
        if !warnings.is_empty() {
            out["warnings"] = json!(warnings);
        }
        if !missing_refs.is_empty() {
            out["missing_code_refs"] = json!(missing_refs);
            out["refs_note"] = json!(
                "these code_refs don't resolve in the repo right now — fix the paths or drop them"
            );
        }
        if !suspects.is_empty() {
            out["suspects"] = json!(suspects);
            out["action_required"] = json!(SUSPECT_ACTION);
        }
        Self::attach_canon(&mut out, &canon);
        Ok(out)
    }

    #[tool(description = "Consolidate duplicates into one survivor: unions \
        tags/code_refs, rehomes the victims' edges, links replaces per victim \
        (archived, still traversable — supersession, not deletion). Pass \
        title/body with your merged text, else the survivor's text stands and \
        the victims' content is dropped. Refused on pinned victims — tell the \
        user. Response is an add_note-style verdict.")]
    pub(crate) async fn merge_nodes(
        &self,
        Parameters(a): Parameters<MergeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.victims.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} victims per call"),
                None,
            ));
        }
        let engine = self.write_engine_for(&a.project)?;
        let outcome = self
            .mcp(&engine)
            .merge_nodes(&a.survivor, &a.victims, a.title, a.body, Source::Agent)
            .map_err(map_err)?;
        let mut out = json!({
            "ok": true,
            "survivor": outcome.survivor.id,
            "merged": outcome.merged,
        });
        if !outcome.warnings.is_empty() {
            out["warnings"] = json!(outcome.warnings);
        }
        if !outcome.missing_refs.is_empty() {
            out["missing_code_refs"] = json!(outcome.missing_refs);
        }
        if !outcome.suspects.is_empty() {
            out["suspects"] = json!(outcome.suspects);
            out["action_required"] = json!(SUSPECT_ACTION);
        }
        Self::attach_canon(&mut out, &outcome.canon);
        self.reply_write(out, &a.project)
    }

    #[tool(description = "Batch update_node for curation sweeps: items apply \
        independently, results positional ({ok, id} | {ok: false, id, error}), \
        every change audited individually.")]
    pub(crate) async fn update_nodes(
        &self,
        Parameters(a): Parameters<UpdateNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.updates.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} updates per call"),
                None,
            ));
        }
        let results: Vec<serde_json::Value> = a
            .updates
            .into_iter()
            .map(|item| {
                let id = item.id.clone();
                self.patch_node(item)
                    .unwrap_or_else(|e| json!({ "ok": false, "id": id, "error": e.message }))
            })
            .collect();
        self.reply_write(json!({ "results": results }), &None)
    }
}
