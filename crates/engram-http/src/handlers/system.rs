//! Core-level routes: health, models, system info, client leases, shutdown, encryption, settings.

use crate::*;

#[derive(Serialize)]
pub(crate) struct GraphResponse {
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Edge>,
}

#[derive(Serialize)]
pub(crate) struct EdgesResponse {
    pub(crate) out: Vec<Edge>,
    #[serde(rename = "in")]
    pub(crate) incoming: Vec<Edge>,
}

pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "version": engram_core::advertised_version(),
        "db": state.db_path,
        // Which process answered: a reconnecting bridge compares it
        // to tell a restarted core from one that was only busy.
        "pid": std::process::id(),
    }))
}

/// The current model selection + presets, or `{"available": false}` when this
/// process has no model-selection hands (library/test embeddings).
pub(crate) async fn models_describe(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match &state.model_admin {
        Some(admin) => Json(admin.describe()),
        None => Json(json!({ "available": false })),
    }
}

/// Apply a model selection: `{"role", "preset"}` or `{"role", "custom"}`.
/// Blocking by design — the response arrives after the download, the load,
/// the live swap, and (for embeddings) the full re-embed have all happened.
pub(crate) async fn models_apply(
    State(state): State<Arc<AppState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Some(admin) = state.model_admin.clone() else {
        return Err(AppError::Core(Error::Project(
            "model selection needs the daemon (engram-alpha serve)".into(),
        )));
    };
    let result = tokio::task::spawn_blocking(move || admin.apply(request))
        .await
        .map_err(|e| AppError::Core(Error::Io(e.to_string())))??;
    Ok(Json(result))
}

/// Settings → System: the doctor's daemon-side facts (version, store health,
/// model cache, wiring) as JSON, best-effort throughout. Launch project only.
pub(crate) async fn system(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let repo = state.repo_root();
    let wiring = engram_core::harness::wiring(&repo);
    let model_cached = engram_core::harness::home_file(".cache/engram").is_some_and(|dir| {
        std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some())
    });
    // Active model identities come from the machine-level selection
    // (~/.engram/models.json); a role without a selection is the default,
    // whose path keeps honoring its ENGRAM_*_DIR override.
    let cortex_cfg = engram_core::cortex::load();
    let dir_str =
        |d: Option<std::path::PathBuf>| d.map(|p| p.display().to_string()).unwrap_or_default();
    let role_info = |role: engram_core::cortex::Role,
                     default_dir: Option<std::path::PathBuf>|
     -> (String, String) {
        let spec = cortex_cfg.effective(role);
        let dir = if cortex_cfg.get(role).is_none() {
            default_dir
        } else {
            engram_core::cortex::cache_dir(&spec.name)
        };
        (spec.name, dir_str(dir))
    };
    let (embed_name, embed_dir) = role_info(
        engram_core::cortex::Role::Embedding,
        engram_core::rag::model_dir(),
    );
    let (rerank_name, rerank_dir) = role_info(
        engram_core::cortex::Role::Reranker,
        engram_core::rag::reranker_model_dir(),
    );
    let (nli_name, nli_dir) = role_info(
        engram_core::cortex::Role::Nli,
        engram_core::nli::nli_model_dir(),
    );
    let db_size = state
        .db_path
        .as_deref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len());

    let engine = state.engine();
    let store = engine.store();
    let stats = store.stats().ok();
    let health = store.health().ok();
    let embed_version = store.embed_version().unwrap_or(0);

    // The process census (core daemons only): who the core is and which
    // clients hold a live lease. `models_state` reports ONNX residency —
    // `loaded` or `unloaded_idle` (idle-unload) — with `since` marking when
    // the current residency began.
    let processes = state.runtime.as_ref().map(|rt| {
        (
            json!({
                "core": {
                    "pid": rt.pid,
                    "version": rt.version,
                    "started_at": rt.started_at,
                    "home": rt.home,
                },
                "clients": rt.clients(),
                // Live MCP session bindings (in-session rebind: 0.8.9 set_project, brief(project) since 0.8.11): where each
                // session IS, which after an in-session rebind is not what
                // its bridge's launch lease says.
                "sessions": state.hub.sessions(),
            }),
            rt.idle.models_state(),
        )
    });

    let mut out = json!({
        "version": engram_core::advertised_version(),
        "daemon": {
            "pid": std::process::id(),
            "uptime_secs": state.started.elapsed().as_secs(),
            "repo_root": repo.display().to_string(),
        },
        "store": {
            "db": state.db_path,
            "size_bytes": db_size,
            "backend": stats.as_ref().map_or("unknown", |s| s.backend),
            "nodes": stats.as_ref().map_or(-1, |s| s.nodes),
            "edges": stats.as_ref().map_or(-1, |s| s.edges),
            "embedded": stats.as_ref().map_or(-1, |s| s.embedded),
            "journal_mode": health.as_ref().and_then(|h| h.journal_mode.clone()).unwrap_or_default(),
            "integrity_ok": health.as_ref().is_some_and(|h| h.integrity_ok),
            "embed_composition": embed_version,
            "embed_composition_current": embed_version >= engram_core::EMBED_COMPOSITION,
        },
        "model_cached": model_cached,
        "reranker": engine.has_reranker(),
        "nli": engine.has_nli(),
        // The local cortex, one row per model with its on-disk home.
        "models": [
            {
                "name": embed_name,
                "role": format!(
                    "embeddings — recall ({}-dim vectors, hybrid search)",
                    engine.embed_model_id().dim
                ),
                "path": embed_dir,
                "active": !engine.embeddings_are_fake(),
            },
            {
                "name": rerank_name,
                "role": "reranker — search precision (cross-encoder)",
                "path": rerank_dir,
                "active": engine.has_reranker(),
            },
            {
                "name": nli_name,
                "role": "NLI — logic (conflict hints, claim checks, Checkup sweeps)",
                "path": nli_dir,
                "active": engine.has_nli(),
            },
        ],
        "model_selection": state.model_admin.is_some(),
        "wiring": wiring,
    });
    if let Some((processes, models_state)) = processes {
        out["processes"] = processes;
        out["models_state"] = models_state;
    }
    Json(out)
}

// ---- client leases + shutdown (core daemons only) ------------------------

#[derive(Deserialize)]
pub(crate) struct RegisterClientBody {
    pub(crate) pid: u32,
    pub(crate) kind: String,
    /// Absolute project root the client is bound to.
    pub(crate) root: String,
    /// The MCP client's name (`clientInfo.name` from the stdio initialize).
    /// Optional: pre-0.8.8 bridges (and registrations racing the handshake)
    /// omit it.
    #[serde(default)]
    pub(crate) client: Option<String>,
}

pub(crate) fn core_runtime(state: &AppState) -> Result<&Arc<CoreRuntime>, AppError> {
    state.runtime.as_ref().ok_or(AppError::NotFound)
}

/// A client (MCP bridge) announces itself: `{pid, kind, root}` → a lease it
/// must renew (`/clients/{lease}/ping`) to stay in the census.
pub(crate) async fn clients_register(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterClientBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let lease = core_runtime(&state)?.register(body.pid, body.kind, body.root, body.client);
    upkeep_project(&state, &lease.root);
    Ok(Json(json!({ "lease_id": lease.lease_id })))
}

/// The first time this core sees a registered project at `root`, bring its
/// installed skills up to this binary: once, off the async runtime, never in
/// an unregistered folder.
pub(crate) fn upkeep_project(state: &Arc<AppState>, root: &str) {
    let Some(admin) = state.skill_admin.clone() else {
        return;
    };
    let now = engram_core::now();
    {
        // Every ping carries its root: a root settled before costs nothing,
        // and one that named no project is looked up again once a minute
        // (it may be registered by then).
        let mut seen = state.upkept.locked();
        match seen.get(root) {
            Some(None) => return,
            Some(Some(miss)) if now - miss < UPKEEP_RETRY_SECS => return,
            _ => {}
        }
        seen.insert(root.to_string(), Some(now));
    }
    let Some(entry) = registry::load()
        .resolve_root(std::path::Path::new(root))
        .cloned()
    else {
        return;
    };
    let mut seen = state.upkept.locked();
    seen.insert(root.to_string(), None);
    // A root inside a project already kept up (a subfolder) adds nothing.
    if entry.root != root && seen.insert(entry.root.clone(), None) == Some(None) {
        return;
    }
    drop(seen);
    tokio::task::spawn_blocking(move || admin.upkeep(std::path::Path::new(&entry.root)));
}

/// How long a bridge root that named no registered project waits before the
/// core looks it up again.
pub(crate) const UPKEEP_RETRY_SECS: i64 = 60;

#[derive(Deserialize)]
pub(crate) struct PingClientBody {
    /// The bridge's current project root — carried on every renewal so a
    /// roots rebind moves the census row (same lease_id) instead of adding
    /// one. Optional: pre-0.8.8 bridges ping with no body.
    pub(crate) root: Option<String>,
    /// The MCP client's name, once the bridge learned it from the stdio
    /// initialize (a fixed-target bridge registers before that handshake).
    #[serde(default)]
    pub(crate) client: Option<String>,
}

/// Renew a lease. 404 = the lease expired (or the core restarted) — the
/// client should register again.
pub(crate) async fn clients_ping(
    State(state): State<Arc<AppState>>,
    Path(lease_id): Path<String>,
    body: Option<Json<PingClientBody>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (root, client) = body.map(|Json(b)| (b.root, b.client)).unwrap_or_default();
    // A roots rebind lands a bridge in another project: same upkeep rule.
    if let Some(root) = &root {
        upkeep_project(&state, root);
    }
    if core_runtime(&state)?.renew(&lease_id, root, client) {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(AppError::NotFound)
    }
}

/// Clean exit: the client withdraws its lease instead of letting it lapse.
pub(crate) async fn clients_unregister(
    State(state): State<Arc<AppState>>,
    Path(lease_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = core_runtime(&state)?.remove(&lease_id);
    Ok(Json(json!({ "removed": removed })))
}

/// Orchestrated stop: reply with what will stop, then (after a beat, so the
/// reply arrives) flip the shutdown watch; the core runs the exit sequence.
pub(crate) async fn shutdown_core(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let rt = core_runtime(&state)?;
    let clients = rt.clients().len();
    let tx = rt.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let _ = tx.send(true);
    });
    Ok(Json(
        json!({ "stopping": true, "pid": rt.pid, "clients": clients }),
    ))
}

// ---- machine-level settings (core daemons only, like the census) ----------

/// One settings reply: the stored selector plus, when it resolves, the
/// project's current name/root so the pane can render it without a join.
#[derive(Deserialize)]
pub(crate) struct EncryptionBody {
    /// "graph" | "history".
    pub(crate) target: String,
    pub(crate) enabled: bool,
}

pub(crate) fn enc_job_key(scope: &Scope, target: &str) -> String {
    format!("{}:{target}", scope.0.as_deref().unwrap_or("@current"))
}

pub(crate) fn enc_job_view(job: Option<&EncJob>) -> serde_json::Value {
    use std::sync::atomic::Ordering;
    match job {
        Some(j) => json!({
            "running": j.running.load(Ordering::Relaxed),
            "done": j.done.load(Ordering::Relaxed),
            "total": j.total.load(Ordering::Relaxed),
            "error": j.error.locked().clone(),
        }),
        None => json!({ "running": false }),
    }
}

/// At-rest encryption status: the machine-level desired switches,
/// each store's OWN recorded state, and any running migration's progress.
/// While a migration runs the engine mutex is busy, so this reads the job's
/// atomics instead of blocking behind it — cheap enough to poll.
pub(crate) async fn get_encryption(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let settings = engram_core::settings::load();
    let jobs = state.enc_jobs.locked().clone();
    let graph_job = jobs.get(&enc_job_key(&scope, "graph"));
    let history_job = jobs.get(&enc_job_key(&scope, "history"));
    let busy = |j: Option<&EncJob>| {
        j.is_some_and(|j| j.running.load(std::sync::atomic::Ordering::Relaxed))
    };
    let (graph_state, history_state) = if busy(graph_job) || busy(history_job) {
        (None, None) // the job view carries the story; don't block on the engine
    } else {
        let engine = state.engine_arc(&scope)?;
        let (g, h) = engine.locked().encryption_states();
        (Some(g), h)
    };
    Ok(Json(json!({
        "graph": {
            "desired": settings.encrypt_graph,
            "state": graph_state.map(engram_core::seal::EncryptionState::as_str),
            "job": enc_job_view(graph_job),
        },
        "history": {
            "desired": settings.encrypt_history,
            "state": history_state.map(engram_core::seal::EncryptionState::as_str),
            "job": enc_job_view(history_job),
        },
    })))
}

/// Flip a machine-wide encryption switch and migrate the current project's
/// store in the background (poll `GET /encryption`). Other projects converge at
/// their next open. Refused while the same target is migrating.
pub(crate) async fn post_encryption(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(body): Json<EncryptionBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    use std::sync::atomic::Ordering;
    if !matches!(body.target.as_str(), "graph" | "history") {
        return Err(AppError::Core(Error::Config(format!(
            "unknown encryption target {:?} — graph | history",
            body.target
        ))));
    }
    let key = enc_job_key(&scope, &body.target);
    {
        let jobs = state.enc_jobs.locked();
        if jobs
            .get(&key)
            .is_some_and(|j| j.running.load(Ordering::Relaxed))
        {
            return Err(AppError::Core(Error::Config(
                "an encryption migration is already running for this target — wait for it".into(),
            )));
        }
    }
    // The switch is machine-global; the migration below applies it to the
    // current project immediately.
    let mut settings = engram_core::settings::load();
    match body.target.as_str() {
        "graph" => settings.encrypt_graph = body.enabled,
        _ => settings.encrypt_history = body.enabled,
    }
    engram_core::settings::save(&settings)?;

    let engine = state.engine_arc(&scope)?;
    let job = EncJob {
        running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ..EncJob::default()
    };
    state.enc_jobs.locked().insert(key, job.clone());
    let (target, enabled) = (body.target.clone(), body.enabled);
    tokio::task::spawn_blocking(move || {
        let result = {
            let mut guard = engine.locked();
            guard.set_desired_encryption(
                Some(settings.encrypt_graph),
                Some(settings.encrypt_history),
            );
            let done = job.done.clone();
            let total = job.total.clone();
            guard.set_store_encryption(&target, enabled, &mut |d, t| {
                done.store(d, Ordering::Relaxed);
                total.store(t, Ordering::Relaxed);
            })
        };
        if let Err(e) = result {
            *job.error.locked() = Some(e.to_string());
        }
        job.running.store(false, Ordering::Relaxed);
    });
    Ok(Json(
        json!({ "started": true, "target": body.target, "enabled": body.enabled }),
    ))
}

pub(crate) fn settings_view(s: &engram_core::settings::Settings) -> serde_json::Value {
    let resolved = s
        .default_agent_project
        .as_deref()
        .and_then(|sel| registry::load().resolve(sel).cloned());
    json!({
        "default_agent_project": s.default_agent_project,
        "default_agent_project_name": resolved.as_ref().map(|e| e.name.clone()),
        "default_agent_project_root": resolved.map(|e| e.root),
        "encrypt_graph": s.encrypt_graph,
        "encrypt_history": s.encrypt_history,
    })
}

/// The machine-level settings (`~/.engram/settings.json`). Core daemons only
/// — a non-core daemon 404s, which is also how an older pane knows to hide
/// the control.
pub(crate) async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    core_runtime(&state)?;
    Ok(Json(settings_view(&engram_core::settings::load())))
}

#[derive(Deserialize)]
pub(crate) struct SettingsBody {
    /// Project id or name; `null`, `""`, or `"home"` clears the setting
    /// (sessions fall to the home graph, today's behavior).
    pub(crate) default_agent_project: Option<String>,
}

/// Write the machine-level settings. The project must exist (registry id or
/// name, or "home") — anything else is a 400 and the file is untouched.
/// Applies to FUTURE bindings only: already-connected sessions keep the
/// project they bound.
pub(crate) async fn post_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    core_runtime(&state)?;
    let stored = match body.default_agent_project.as_deref() {
        None | Some("" | registry::HOME_PROJECT) => None,
        Some(sel) => {
            let entry = registry::load().resolve(sel).cloned().ok_or_else(|| {
                AppError::Core(Error::Project(format!(
                    "unknown project '{sel}' — register it first (engram-alpha serve), or pass \"home\""
                )))
            })?;
            // Stored as the stable id: renames of the human slug can't strand
            // the setting, and the bridge's registry lookup takes either.
            Some(entry.id)
        }
    };
    let mut settings = engram_core::settings::load();
    settings.default_agent_project = stored;
    engram_core::settings::save(&settings)?;
    Ok(Json(settings_view(&settings)))
}

// ---- project registry ----------------------------------------
