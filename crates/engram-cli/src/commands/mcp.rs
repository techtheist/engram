//! `mcp`: the stdio bridge and how it picks its project.

use crate::*;

/// The repo root a store path belongs to: the parent of its `.engram` dir.
/// Path-shaped only — the store file itself need not exist yet.
pub(crate) fn repo_root_of(db: &Path) -> Option<PathBuf> {
    let abs = if db.is_absolute() {
        db.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(db)
    };
    let dir = abs.parent()?;
    if dir.file_name()? != ".engram" {
        return None;
    }
    let root = dir.parent()?;
    Some(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
}

/// Where an MCP session for `db` connects: a daemon launched from this repo,
/// else the machine core with the repo registered (so the scoped endpoint binds
/// this project), else `None`.
pub(crate) fn resolve_mcp_target(db: &Path) -> Option<String> {
    if let Some(port) = daemon_for(db) {
        return Some(format!("http://127.0.0.1:{port}/mcp"));
    }
    let port = machine_core()?;
    let root = repo_root_of(db)?;
    let body = serde_json::json!({ "path": root }).to_string();
    let id = doctor::http_post(port, "/projects", &body)
        .and_then(|resp| serde_json::from_str::<serde_json::Value>(&resp).ok())
        .and_then(|v| v["id"].as_str().map(str::to_string))?;
    Some(format!("http://127.0.0.1:{port}/projects/{id}/mcp"))
}

/// The path an omitted `--db` used to mean: the cwd's repo graph.
pub(crate) fn default_db_path() -> PathBuf {
    PathBuf::from(".engram/graph.db")
}

/// The bail every failed resolution shares: where to look and what to run.
pub(crate) fn no_core_error(what: &std::path::Path) -> anyhow::Error {
    anyhow::anyhow!(
        "no engram core is reachable for {} and starting one didn't succeed — this MCP \
         server is a light bridge and never opens the store itself. See {} for why the \
         core didn't come up, or start it yourself: `engram-alpha serve`.",
        what.display(),
        registry::engram_home()
            .map(|h| h.join("core.log").display().to_string())
            .unwrap_or_else(|| "~/.engram/core.log".into())
    )
}

pub(crate) async fn run_mcp(args: McpArgs) -> anyhow::Result<()> {
    // This process is ALWAYS a bridge (0.8.8, generalizing the 0.8.5 tepin
    // rule to every backend): the core owns every store; mcp never opens one.
    match &args.db {
        // An explicit --db is an explicit answer: fixed project, roots
        // ignored, verbatim passthrough (the pre-roots doctrine).
        Some(raw) => run_mcp_fixed(raw, args.fake_embeddings).await,
        // No --db: bind by the client's MCP roots, falling back to cwd —
        // one global config entry serves every project (issue #4).
        None => run_mcp_roots(args.fake_embeddings, args.wired_only).await,
    }
}

pub(crate) async fn run_mcp_fixed(raw_db: &Path, fake_embeddings: bool) -> anyhow::Result<()> {
    let db = engram_core::resolve_db_path(raw_db);
    // mcp.log is append-mode and shared by every bridge on this repo — the
    // pid is what tells concurrent sessions apart.
    tracing::info!(
        "engram-alpha mcp v{} (pid {}, db: {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        db.display()
    );
    // The repo's .engram/ dir must exist for project registration to resolve
    // — the store itself is the core's to create on first open.
    if let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(dir);
    }
    // Census lease: the core's /system names this bridge (pid, project root)
    // while it lives. Pure observability — never a bridging dependency.
    let lease_root = repo_root_of(&db)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
        .display()
        .to_string();
    // The resolver runs after the stdio transport serves, so a slow or failed
    // core boot answers held requests with an error instead of silence.
    let resolve: engram_mcp::FixedResolver = Arc::new(move || {
        let target = |url: String| engram_mcp::ResolvedTarget {
            url,
            lease_root: lease_root.clone(),
            bound_by: engram_mcp::BoundBy::Db,
        };
        // Version handshake before the first resolution — resolving against
        // an older core would silently pin this session to it.
        retire_older_core();
        if let Some(url) = resolve_mcp_target(&db) {
            tracing::info!("bridging stdio MCP to {url} (db: {})", db.display());
            return Ok(target(url));
        }
        let patience = match ensure_machine_core(fake_embeddings) {
            // A freshly spawned core can be slow to answer its first
            // requests (boot-time loads block the runtime for seconds) —
            // retry the resolution instead of failing the session on one
            // slow probe.
            Ok(_) => std::time::Duration::from_secs(20),
            // A failed spawn only warns: a repo-local daemon can still
            // resolve — the loop below gets exactly one attempt.
            Err(e) => {
                tracing::warn!("core spawn failed: {e:#}");
                std::time::Duration::ZERO
            }
        };
        let deadline = std::time::Instant::now() + patience;
        loop {
            if let Some(url) = resolve_mcp_target(&db) {
                tracing::info!("bridging stdio MCP to {url} (db: {})", db.display());
                return Ok(target(url));
            }
            if std::time::Instant::now() >= deadline {
                // Better no tools than a stolen lock — and better a
                // clear error in mcp.log than a silently heavy in-process
                // hub.
                return Err(no_core_error(&db));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
    engram_mcp::serve_stdio_bridge_with(
        engram_mcp::BridgeTarget::Fixed { resolve },
        Some(startup_probe(fake_embeddings)),
    )
    .await
}

/// What a starting core is doing, readable from outside (Insight
/// 00dxh3ev2j1f): a first run downloads ~450 MB of models before listening, so
/// an early tool call learns "downloading, retry".
pub(crate) fn startup_probe(fake_embeddings: bool) -> engram_mcp::StartupProbe {
    Arc::new(move || {
        if fake_embeddings {
            return None;
        }
        use engram_core::cortex::Role;
        let cfg = engram_core::cortex::load();
        let (mut parts, mut downloading) = (Vec::new(), false);
        for role in [Role::Embedding, Role::Reranker, Role::Nli] {
            let spec = cfg.effective(role);
            let (dir, files): (Option<PathBuf>, Vec<String>) =
                if role == Role::Nli && is_default_spec(role, &spec) {
                    (
                        engram_core::nli::nli_model_dir(),
                        engram_core::nli::NLI_MODEL_FILES
                            .iter()
                            .map(std::string::ToString::to_string)
                            .collect(),
                    )
                } else {
                    (
                        engram_core::cortex::cache_dir(&spec.name),
                        engram_core::cortex::spec_files(role, &spec)
                            .into_iter()
                            .map(|(name, _)| name)
                            .collect(),
                    )
                };
            let Some(dir) = dir else { continue };
            if files.iter().all(|f| dir.join(f).is_file()) {
                parts.push(format!("{} ready", spec.name));
                continue;
            }
            downloading = true;
            // Finished files plus the `.part` curl is writing right now.
            let bytes: u64 = std::fs::read_dir(&dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|e| e.metadata().ok())
                        .filter(std::fs::Metadata::is_file)
                        .map(|m| m.len())
                        .sum()
                })
                .unwrap_or(0);
            parts.push(if bytes > 0 {
                format!("{} {} MB so far", spec.name, bytes / 1_000_000)
            } else {
                format!("{} waiting", spec.name)
            });
        }
        let log = registry::engram_home()
            .map(|h| format!(" Progress is logged in {}.", h.join("core.log").display()))
            .unwrap_or_default();
        Some(if downloading {
            format!(
                "First run: it is downloading its local models (about 450 MB with the \
                 defaults) — {}.{log}",
                parts.join("; ")
            )
        } else {
            format!("It is loading its local models.{log}")
        })
    })
}

pub(crate) async fn run_mcp_roots(fake_embeddings: bool, wired_only: bool) -> anyhow::Result<()> {
    tracing::info!(
        "engram-alpha mcp v{} (pid {}, no --db — binding by client roots, then cwd, \
         then the default agent project, then home{})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        if wired_only {
            "; wired projects only"
        } else {
            ""
        },
    );
    // IDE launchers spawn MCP servers from arbitrary cwds — `/`, a vanished
    // dir (the Windsurf JetBrains plugin does). A missing cwd must not kill
    // the bridge before roots can name the real project; "/" stands in and
    // the resolver's usability check decides what it can host.
    let fallback_root =
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(std::path::MAIN_SEPARATOR_STR));
    let resolve: engram_mcp::RootResolver = Arc::new(move |root: PathBuf| {
        // --wired-only never turns a folder into a project: inside a registered
        // project binds its root, anything else reads home with writes refused.
        let root = if wired_only && root_cannot_host(&root).is_none() {
            match wired_root(&root) {
                Some(wired) => wired,
                None => {
                    tracing::info!(
                        "{} is not an Engram project (--wired-only) — binding the home graph, \
                         project writes refused until it is wired",
                        root.display()
                    );
                    let deadline = std::time::Instant::now()
                        + match ensure_machine_core(fake_embeddings) {
                            Ok(_) => std::time::Duration::from_secs(20),
                            Err(e) => {
                                tracing::warn!("core spawn failed: {e:#}");
                                std::time::Duration::ZERO
                            }
                        };
                    loop {
                        if let Some(port) = machine_core() {
                            return Ok(engram_mcp::ResolvedTarget {
                                url: format!("http://127.0.0.1:{port}/mcp"),
                                lease_root: root.display().to_string(),
                                bound_by: engram_mcp::BoundBy::Unwired,
                            });
                        }
                        if std::time::Instant::now() >= deadline {
                            return Err(no_core_error(&root));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                }
            }
        } else {
            root
        };
        // Ensure the core here, after the handshake; single-flight, so a racing
        // rebind finds the running core.
        let patience = match ensure_machine_core(fake_embeddings) {
            // A freshly spawned core can be slow to answer its first
            // requests (boot-time loads block the runtime for seconds) —
            // give the per-root resolution 20s of patience instead of
            // failing the whole session on one slow health probe.
            Ok(_) => std::time::Duration::from_secs(20),
            // A failed spawn only warns: a repo-local daemon can still
            // resolve — the loops below get exactly one attempt.
            Err(e) => {
                tracing::warn!("core spawn failed: {e:#}");
                std::time::Duration::ZERO
            }
        };
        let deadline = std::time::Instant::now() + patience;
        // The root's .engram/ dir must exist for project registration to
        // resolve — the store itself is the core's to create on first open.
        let dot = root.join(".engram");
        let cannot_host = if let Some(why) = root_cannot_host(&root) {
            Some(why)
        } else if std::fs::create_dir_all(&dot).is_err() && !dot.is_dir() {
            Some("mkdir .engram failed".to_string())
        } else {
            None
        };
        if let Some(why) = cannot_host {
            // The root can't host a project (`/`, a deleted dir, `~`; issue #11): bind
            // the default agent project, else the home graph. A later roots change
            // rebinds normally.
            tracing::warn!(
                "root {} can't host a project ({why}) — trying the default agent project \
                 setting, then the home graph",
                root.display()
            );
            let home = registry::engram_home()
                .map(|h| h.display().to_string())
                .unwrap_or_else(|| "~/.engram".into());
            loop {
                if let Some(port) = machine_core() {
                    if let Some(target) = default_agent_target(port) {
                        return Ok(target);
                    }
                    tracing::info!(
                        "binding the home graph (no default agent project configured) — \
                         reads only until the agent binds the session with a scoped `brief`"
                    );
                    return Ok(engram_mcp::ResolvedTarget {
                        url: format!("http://127.0.0.1:{port}/mcp"),
                        lease_root: home,
                        bound_by: engram_mcp::BoundBy::Home,
                    });
                }
                if std::time::Instant::now() >= deadline {
                    return Err(no_core_error(&root));
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        let db = engram_core::resolve_db_path(&root.join(".engram/graph.db"));
        loop {
            if let Some(url) = resolve_mcp_target(&db) {
                return Ok(engram_mcp::ResolvedTarget {
                    url,
                    lease_root: root.display().to_string(),
                    bound_by: engram_mcp::BoundBy::Cwd,
                });
            }
            if std::time::Instant::now() >= deadline {
                return Err(no_core_error(&root));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
    engram_mcp::serve_stdio_bridge_with(
        engram_mcp::BridgeTarget::Roots {
            fallback_root,
            resolve,
        },
        Some(startup_probe(fake_embeddings)),
    )
    .await
}

/// The `default_agent_project` rung, fetched from the core at bind time (so a
/// change affects future bindings only). `None` when unset, "home" or no
/// longer registered.
pub(crate) fn default_agent_target(port: u16) -> Option<engram_mcp::ResolvedTarget> {
    let raw = doctor::http_get(port, "/settings")?;
    let parsed = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    let sel = parsed["default_agent_project"].as_str()?.to_string();
    if sel.is_empty() || sel == registry::HOME_PROJECT {
        return None;
    }
    let Some(entry) = registry::load().resolve(&sel).cloned() else {
        tracing::warn!(
            "default agent project '{sel}' is not on the registry any more — falling to the \
             home graph"
        );
        return None;
    };
    tracing::info!(
        "binding the default agent project '{}' ({}) — the machine-level setting for \
         sessions that can't reveal their folder",
        entry.name,
        entry.root
    );
    Some(engram_mcp::ResolvedTarget {
        url: format!("http://127.0.0.1:{port}/projects/{}/mcp", entry.id),
        lease_root: entry.root,
        bound_by: engram_mcp::BoundBy::DefaultProject,
    })
}

/// The Engram project a folder belongs to, for `mcp --wired-only`: the
/// folder itself when it holds `.engram/`, else the root of the registered
/// project containing it (longest root wins). None = not wired.
pub(crate) fn wired_root(folder: &Path) -> Option<PathBuf> {
    if folder.join(".engram").is_dir() {
        return Some(folder.to_path_buf());
    }
    registry::load()
        .resolve_root(folder)
        .map(|entry| PathBuf::from(&entry.root))
        .filter(|root| root_cannot_host(root).is_none())
}

/// Never a project root (issue #11): `/` and the home directory, where IDE
/// plugins launch bridges. Returns the reason, or `None` when it may host one.
pub(crate) fn root_cannot_host(root: &Path) -> Option<String> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if root.parent().is_none() {
        return Some("the filesystem root is never a project".into());
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let home = home.canonicalize().unwrap_or(home);
    (root == home)
        .then(|| "the home directory is never a project — the home graph lives there".into())
}
