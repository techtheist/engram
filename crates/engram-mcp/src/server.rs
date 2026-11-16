//! The `Engram` server: session binding, reply shaping, and the MCP handler.

use crate::*;

impl Engram {
    pub fn new(engine: Engine) -> Self {
        Self::with_hub(Arc::new(Hub::single(engine)))
    }

    /// Build over an engine shared with the HTTP server (same DB + listener).
    pub fn with_shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self::with_hub(Arc::new(Hub::single_shared(engine)))
    }

    /// The full multi-project form: the same hub the HTTP server holds.
    pub fn with_hub(hub: Arc<Hub>) -> Self {
        let engine = hub.current_engine();
        let session_id: Arc<str> = format!("mcp-{}", engram_core::id::new_id()).into();
        hub.session_bind(&session_id, None);
        Self {
            binding: Arc::new(std::sync::RwLock::new(Binding {
                trace: SessionTrace::start(&engine, &session_id, None),
                engine,
                bound: None,
                bound_by: BoundBy::Route,
                client: None,
                writes: 0,
            })),
            conflicts: Arc::new(Mutex::new(hub.subscribe_conflicts())),
            _registration: Arc::new(SessionRegistration {
                hub: hub.clone(),
                session_id: session_id.clone(),
            }),
            hub,
            session_id,
        }
    }

    /// Bound to one project of the hub (v0.6.2 machine core): `selector`'s
    /// engine is this session's current project — what an omitted `project`
    /// param means — regardless of which project the core launched with.
    pub fn for_project(hub: Arc<Hub>, selector: &str) -> Result<Self, engram_core::Error> {
        let engine = hub.get(selector)?;
        let session_id: Arc<str> = format!("mcp-{}", engram_core::id::new_id()).into();
        hub.session_bind(&session_id, Some(selector));
        Ok(Self {
            binding: Arc::new(std::sync::RwLock::new(Binding {
                trace: SessionTrace::start(
                    &engine,
                    &session_id,
                    Some(format!("project {selector}")),
                ),
                engine,
                bound: Some(selector.into()),
                bound_by: BoundBy::Route,
                client: None,
                writes: 0,
            })),
            conflicts: Arc::new(Mutex::new(hub.subscribe_conflicts())),
            _registration: Arc::new(SessionRegistration {
                hub: hub.clone(),
                session_id: session_id.clone(),
            }),
            hub,
            session_id,
        })
    }

    /// This session's engine right now; never cache it. Re-resolving through the
    /// hub each call follows a store replaced on disk (issue #8) instead of
    /// serving the orphaned file.
    pub(crate) fn session_engine(&self) -> Arc<Mutex<Engine>> {
        let (engine, bound) = {
            let b = self.binding.reading();
            (b.engine.clone(), b.bound.clone())
        };
        let Some(sel) = bound else {
            return engine;
        };
        let Ok(fresh) = self.hub.get(&sel) else {
            return engine;
        };
        if Arc::ptr_eq(&fresh, &engine) {
            return engine;
        }
        let trace = SessionTrace::start(
            &fresh,
            &self.session_id,
            Some(format!(
                "project {sel} reopened after its store was replaced"
            )),
        );
        let mut b = self.binding.writing();
        // A concurrent call may have swapped (or a brief rebound) already.
        if Arc::ptr_eq(&b.engine, &engine) {
            b.engine = fresh.clone();
            b.trace = trace;
        }
        b.engine.clone()
    }

    /// The first line of a scoped `brief` (Problem 00e354dmopga). Moving away from
    /// a project the session really worked in leads with a warning naming both
    /// graphs and the way back; leaving a fallback binding or re-briefing the same
    /// project is a plain confirmation.
    pub(crate) fn rebind_line(&self, to: &str, from: &str, from_rung: BoundBy) -> String {
        let label = |id: &str| {
            if id == registry::HOME_PROJECT {
                "the home graph".to_string()
            } else {
                format!("'{}'", self.hub.project_name(id))
            }
        };
        let fallback = matches!(
            from_rung,
            BoundBy::Home | BoundBy::Unwired | BoundBy::DefaultProject
        );
        if to == from || fallback {
            return format!("_Session bound to {}._", label(to));
        }
        let back = self
            .hub
            .project_root(from)
            .unwrap_or_else(|| self.hub.project_name(from));
        format!(
            "**Warning — this session's writes now go to {to_label}**, no longer to {from_label}. \
             A scoped brief rebinds; it is not a read-only peek. If you only meant to read \
             {to_label}, go back now with brief(project: \"{back}\") — reads of a sibling \
             take `project` on search/list_open/get_node without moving the session.",
            to_label = label(to),
            from_label = label(from),
        )
    }

    pub(crate) fn session_bound(&self) -> Option<Arc<str>> {
        self.binding.reading().bound.clone()
    }

    /// Resolve a tool's optional `project` selector: omitted = the current
    /// project, a name/id = that registered project, `home` = the user-level
    /// home graph. `all` never resolves to one engine — the hub's refusal
    /// explains where fan-out reads and shared writes belong.
    pub(crate) fn engine_for(
        &self,
        project: &Option<String>,
    ) -> Result<Arc<Mutex<Engine>>, ErrorData> {
        match project.as_deref() {
            None => Ok(self.session_engine()),
            Some(sel) => self.hub.get(sel).map_err(map_err),
        }
    }

    /// [`engine_for`](Self::engine_for) for writes (issue #11): a session bound to
    /// home only by fallback refuses writes to "this project" and teaches the
    /// binding call. Reads still work; an explicit `project` passes.
    pub(crate) fn write_engine_for(
        &self,
        project: &Option<String>,
    ) -> Result<Arc<Mutex<Engine>>, ErrorData> {
        if project.is_none() && self.bound_by().refuses_writes() {
            return Err(ErrorData::invalid_params(
                self.unbound_write_refusal(),
                None,
            ));
        }
        self.engine_for(project)
    }

    pub(crate) fn bound_by(&self) -> BoundBy {
        self.binding.reading().bound_by
    }

    /// What the initialize's `clientInfo` says about this session: the end
    /// client's name, and — from an engram bridge — the ladder rung. A
    /// scoped `brief` that already ran keeps its `Brief` rung.
    pub fn record_client(&self, info: &Implementation) {
        let name = info.name.trim().to_string();
        let client = (!name.is_empty()).then_some(name);
        let bound_by = BoundBy::from_description(info.description.as_deref());
        {
            let mut b = self.binding.writing();
            if client.is_some() {
                b.client = client.clone();
            }
            if let Some(rung) = bound_by
                && b.bound_by != BoundBy::Brief
            {
                b.bound_by = rung;
            }
        }
        self.hub.session_meta(
            &self.session_id,
            client,
            bound_by.unwrap_or(BoundBy::Route).as_str(),
        );
    }

    pub(crate) fn session_client(&self) -> Option<String> {
        self.binding.reading().client.clone()
    }

    /// The teaching error behind a refused write: who the client is, why the
    /// session landed here, the exact call that fixes it, and the roster to
    /// pick from.
    pub(crate) fn unbound_write_refusal(&self) -> String {
        let client = self
            .session_client()
            .unwrap_or_else(|| "your MCP client".into());
        let roster: Vec<String> = self
            .hub
            .projects_for(self.session_bound().as_deref())
            .into_iter()
            .filter(|p| !p.home)
            .map(|p| match &p.root {
                Some(root) => format!("{} ({root})", p.name),
                None => p.name.clone(),
            })
            .collect();
        let roster = if roster.is_empty() {
            "none registered yet — `engram-alpha serve` inside the repo registers it".to_string()
        } else {
            roster.join(", ")
        };
        if self.bound_by() == BoundBy::Unwired {
            return format!(
                "write refused: this workspace is not an Engram project yet, so the session \
                 reads the home graph and won't write into it by accident. To remember this \
                 project, wire it — `/engram:setup` in Claude Code, or `engram-alpha setup` in \
                 the repository root — then reconnect (`/mcp`) and retry. To write user-level \
                 knowledge deliberately, pass project \"home\"; to write into another project, \
                 pass its name. Registered projects: {roster}."
            );
        }
        format!(
            "write refused: this session is bound to the home graph by fallback — {client} \
             answered no MCP roots and the bridge's launch directory can't host a project, so \
             engram doesn't know which workspace you are in (a resumed or restarted session \
             starts here again even if you bound it earlier). Call `brief` with `project` set \
             to your workspace's ABSOLUTE path, then retry this write; `brief` with \
             project \"home\" writes the user-level graph deliberately. Registered projects: \
             {roster}."
        )
    }

    /// The display name of the graph a write went to: the session's own
    /// project when `project` was omitted, else the selector resolved.
    pub(crate) fn project_label(&self, project: &Option<String>) -> String {
        let id = match project.as_deref() {
            Some(sel) => match self.hub.resolve_id(sel) {
                Ok(id) => id,
                Err(_) => return sel.to_string(),
            },
            None => match self.session_bound() {
                Some(b) => self.hub.resolve_id(&b).unwrap_or_else(|_| b.to_string()),
                None => self.hub.current().id.clone(),
            },
        };
        self.hub.project_name(&id)
    }

    /// Stamp a write verdict with its `project` (issue #11), plus a one-time
    /// `binding_note` on the first write of a session bound by the default-project
    /// fallback.
    pub(crate) fn stamp_write(&self, out: &mut serde_json::Value, project: &Option<String>) {
        if !out.is_object() {
            return;
        }
        out["project"] = json!(self.project_label(project));
        let (bound_by, first) = {
            let mut b = self.binding.writing();
            let first = b.writes == 0;
            b.writes = b.writes.saturating_add(1);
            (b.bound_by, first)
        };
        if first && project.is_none() && bound_by == BoundBy::DefaultProject {
            out["binding_note"] = json!(format!(
                "this session is bound to '{}' by the machine-level default-agent-project \
                 setting, not by your workspace ({} answered no MCP roots). If that is not \
                 the project you are working in, call `brief` with `project` set to your \
                 workspace's absolute path — this note rides the first write only.",
                out["project"].as_str().unwrap_or_default(),
                self.session_client()
                    .unwrap_or_else(|| "the MCP client".into())
            ));
        }
    }

    /// [`reply`](Self::reply) for writes: stamps the verdict first.
    pub(crate) fn reply_write(
        &self,
        mut out: serde_json::Value,
        project: &Option<String>,
    ) -> Result<CallToolResult, ErrorData> {
        self.stamp_write(&mut out, project);
        self.reply(&out)
    }

    /// Apply a search `detail` level: compact strips snippets and neighbors,
    /// snippet is the wire shape as-is, full attaches each hit's body.
    pub(crate) fn shape_hits(
        &self,
        detail: &Detail,
        hits: Vec<engram_core::SearchHit>,
        engine: Option<&Arc<Mutex<Engine>>>,
    ) -> serde_json::Value {
        match detail {
            Detail::Snippet => json!(hits),
            Detail::Compact => json!(
                hits.iter()
                    .map(|h| {
                        let mut o = json!({
                            "id": h.id,
                            "type": h.node_type,
                            "title": h.title,
                            "score": h.score,
                            "trust": h.trust,
                            "stale": h.stale,
                        });
                        if let Some(p) = &h.project {
                            o["project"] = json!(p);
                        }
                        if let Some(s) = h.status {
                            o["status"] = json!(s);
                        }
                        if let Some(st) = &h.standing {
                            o["standing"] = json!(st.verdict);
                        }
                        o
                    })
                    .collect::<Vec<_>>()
            ),
            Detail::Full => json!(
                hits.iter()
                    .map(|h| {
                        let mut o = json!(h);
                        let resolved = match (&h.project, engine) {
                            (Some(p), _) => self.hub.get(p).ok(),
                            (None, Some(e)) => Some((*e).clone()),
                            (None, None) => Some(self.session_engine()),
                        };
                        if let Some(e) = resolved
                            && let Ok(Some(node)) = e.locked().get_node(&h.id)
                        {
                            o["body"] = json!(node.body);
                            o["tags"] = json!(node.tags);
                            o["code_refs"] = json!(node.code_refs);
                        }
                        o
                    })
                    .collect::<Vec<_>>()
            ),
        }
    }

    /// The mid-session conflict push, delivery side: whatever judged
    /// contradictions landed since this session's last tool call, formatted
    /// as one out-of-band text block. Titles are resolved here — at delivery
    /// time, with short engine locks — never inside the emitting listener.
    pub(crate) fn drain_conflict_alerts(&self) -> Option<String> {
        let alerts = self.conflicts.locked().drain();
        if alerts.is_empty() {
            return None;
        }
        let title_of = |project: &str, id: &str| -> String {
            self.hub
                .get(project)
                .ok()
                .and_then(|e| e.locked().get_node(id).ok().flatten())
                .map_or_else(|| id.to_string(), |n| n.title)
        };
        let mut lines = vec![format!(
            "MEMORY ALERT: {} contradiction(s) were judged while you worked — check whether they touch your current task before relying on either side.",
            alerts.len()
        )];
        for a in alerts.iter().take(3) {
            lines.push(format!(
                "- \"{}\" now conflicts-with \"{}\" [project {}, edge {}] — get_node either side for the full claims.",
                title_of(&a.project, &a.from_id),
                title_of(&a.project, &a.to_id),
                a.project,
                a.edge_id
            ));
        }
        if alerts.len() > 3 {
            lines.push(format!(
                "- (+{} more — see list_suspects / the pane Review drawer)",
                alerts.len() - 3
            ));
        }
        Some(lines.join("\n"))
    }

    /// The standard tool reply: the JSON payload, plus any pending conflict
    /// alert as a second content block — every tool call is a delivery
    /// window, so the push needs no client or bridge support at all.
    pub(crate) fn reply<T: Serialize>(&self, v: &T) -> Result<CallToolResult, ErrorData> {
        let mut result = ok_json(v)?;
        if let Some(alert) = self.drain_conflict_alerts() {
            result.content.push(ContentBlock::text(alert));
        }
        Ok(result)
    }

    /// Decorate a write response with the canon check's verdicts and the
    /// action note they call for (shared by add_note and update_node).
    pub(crate) fn attach_canon(out: &mut serde_json::Value, canon: &[engram_core::CanonVerdict]) {
        if canon.is_empty() {
            return;
        }
        out["canon"] = json!(canon);
        out["canon_note"] = json!(if canon.iter().any(|c| c.verdict == "contradicts") {
            "existing canon CONTRADICTS this text (see `canon`) — read the flagged node; \
             if the disagreement is real, link conflicts-with and tell the user"
        } else {
            "existing canon already supports this text (see `canon`) — consider linking it \
             (because / builds-on) instead of leaving the reinforcement implicit"
        });
    }

    /// This session as the writer, for the engine calls that take their own
    /// locks (the checked writes). Keeps the census row alive like [`mcp`].
    pub(crate) fn writer(&self) -> engram_core::AuditOrigin {
        self.hub.session_touch(&self.session_id);
        engram_core::AuditOrigin::mcp(self.session_id.to_string())
    }

    /// Lock a scoped engine with this session stamped as the writer. Every
    /// tool passes through here, so it also keeps the session's census row
    /// alive (lazy-expired like leases).
    pub(crate) fn mcp<'a>(
        &self,
        engine: &'a Arc<Mutex<Engine>>,
    ) -> std::sync::MutexGuard<'a, Engine> {
        self.hub.session_touch(&self.session_id);
        let mut guard = engine.locked();
        guard.set_audit_origin(engram_core::AuditOrigin::mcp(self.session_id.to_string()));
        guard
    }

    /// A line atop the unscoped brief when the binding came from a fallback rung:
    /// the agent may be the only party that knows the real workspace (issue #4).
    pub(crate) fn fallback_hint(&self) -> Option<String> {
        if !self.hub.is_multi() {
            return None;
        }
        let id = match self.session_bound() {
            Some(b) => self.hub.resolve_id(&b).ok()?,
            None => self.hub.current().id.clone(),
        };
        let bound_by = self.bound_by();
        // The bridge said so, or — for a bridge that doesn't
        // announce its rung — the graph itself gives it away.
        let default = engram_core::settings::load().default_agent_project;
        let on_fallback = matches!(
            bound_by,
            BoundBy::Home | BoundBy::DefaultProject | BoundBy::Unwired
        ) || (bound_by == BoundBy::Route
            && (id == registry::HOME_PROJECT || default.as_deref() == Some(id.as_str())));
        if !on_fallback {
            return None;
        }
        let client = self
            .session_client()
            .unwrap_or_else(|| "your MCP client".into());
        if bound_by == BoundBy::Unwired {
            return Some(
                "_This workspace is not an Engram project yet — the session reads the home \
                 graph and REFUSES writes addressed to this project. If the user wants this \
                 project remembered, offer to wire it (`/engram:setup` in Claude Code, or \
                 `engram-alpha setup` in the repository root), then reconnect._"
                    .to_string(),
            );
        }
        Some(if bound_by.refuses_writes() {
            format!(
                "_This session is bound to the home graph by fallback: {client} answered no \
                 MCP roots and the bridge's launch directory can't host a project. WRITES \
                 ARE REFUSED until you bind it — call `brief` again with `project` set to \
                 your workspace's absolute path (it rebinds this session and returns that \
                 project's brief), or with project \"home\" to work in the user-level graph \
                 deliberately._"
            )
        } else {
            format!(
                "_This session is bound to '{id}' by fallback. If your workspace is a \
                 different project, call `brief` again with `project` set to your \
                 workspace's absolute path — it rebinds this session and returns \
                 that project's brief._"
            )
        })
    }
}

/// How many concrete node resources `resources/list` advertises (newest
/// first); the full graph stays reachable through the uri template.
pub(crate) const RESOURCE_LIST_CAP: usize = 25;

/// The identity every engram MCP endpoint advertises. Shared by the real
/// server and the roots-mode bridge, which must answer the stdio initialize
/// before its upstream exists — same crate, same contract.
pub(crate) fn engram_server_info() -> ServerInfo {
    ServerInfo::new(
        ServerCapabilities::builder()
            .enable_tools()
            // A bridge that reconnects to a restarted (maybe newer) core
            // says so with notifications/tools/list_changed.
            .enable_tool_list_changed()
            .enable_resources()
            .build(),
    )
    .with_server_info(Implementation::new("engram", env!("CARGO_PKG_VERSION")))
    .with_instructions(INSTRUCTIONS.to_string())
}

#[tool_handler]
impl ServerHandler for Engram {
    fn get_info(&self) -> ServerInfo {
        engram_server_info()
    }

    /// The handshake tells a session who it serves (issue #11): a bridge sends the
    /// end client's name and `bound_by=<rung>`; a direct client names itself.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        self.record_client(&request.client_info);
        context.peer.set_peer_info(request);
        Ok(self.get_info())
    }

    /// Appendix A: `engram://node/{id}` so a user can @-mention a node in a
    /// prompt. The list shows the newest nodes; anything else resolves
    /// through the template with an id from `search`/the pane.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let engine = self.session_engine();
        let nodes = self
            .mcp(&engine)
            .store()
            .recent_nodes(RESOURCE_LIST_CAP)
            .map_err(map_err)?;
        Ok(ListResourcesResult {
            meta: None,
            next_cursor: None,
            resources: nodes
                .into_iter()
                .map(|n| {
                    Resource::new(format!("engram://node/{}", n.id), n.id.clone())
                        .with_title(n.title)
                        .with_description(format!(
                            "{} node in the Engram memory graph",
                            n.node_type.as_str()
                        ))
                        .with_mime_type("application/json")
                })
                .collect(),
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult {
            meta: None,
            next_cursor: None,
            resource_templates: vec![
                ResourceTemplate::new("engram://node/{id}", "node")
                    .with_title("Engram memory node")
                    .with_description(
                        "One memory node with its edges, by id (ids come from search, \
                         the brief, or the pane)",
                    )
                    .with_mime_type("application/json"),
            ],
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let Some(id) = request.uri.strip_prefix("engram://node/") else {
            return Err(ErrorData::invalid_params(
                format!("unknown resource uri: {}", request.uri),
                None,
            ));
        };
        let engine = self.session_engine();
        let engine = self.mcp(&engine);
        let Some(node) = engine.get_node(id).map_err(map_err)? else {
            return Err(ErrorData::invalid_params(
                format!("node not found: {id}"),
                None,
            ));
        };
        let out = engine.edges_out(id).map_err(map_err)?;
        let incoming = engine.edges_in(id).map_err(map_err)?;
        let payload = json!({ "node": node, "edges_out": out, "edges_in": incoming });
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(payload.to_string(), request.uri)
                .with_mime_type("application/json"),
        ]))
    }
}

/// Progressive disclosure for search results (v0.6.3): compact → snippet →
/// full, so the assistant starts cheap and expands on demand.
pub(crate) enum Detail {
    Compact,
    Snippet,
    Full,
}

impl Detail {
    pub(crate) fn parse(s: Option<&str>) -> Result<Self, ErrorData> {
        match s {
            None | Some("snippet") => Ok(Detail::Snippet),
            Some("compact") => Ok(Detail::Compact),
            Some("full") => Ok(Detail::Full),
            Some(other) => Err(ErrorData::invalid_params(
                format!("detail {other:?}: use compact | snippet | full"),
                None,
            )),
        }
    }
}

/// Serve the MCP protocol over stdio until the client disconnects.
pub async fn serve_stdio(engine: Engine) -> anyhow::Result<()> {
    serve(Engram::new(engine)).await
}

/// Serve over stdio using an engine shared with the HTTP server.
pub async fn serve_stdio_shared(engine: Arc<Mutex<Engine>>) -> anyhow::Result<()> {
    serve(Engram::with_shared(engine)).await
}

pub(crate) async fn serve(server: Engram) -> anyhow::Result<()> {
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
