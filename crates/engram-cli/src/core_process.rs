//! `core`: the machine core process itself.

use crate::*;

/// The machine core: the one process holding every store, the models, the
/// pane and the MCP endpoints. SIGTERM, ctrl-c and `POST /shutdown` all run the
/// same orchestrated exit.
pub(crate) async fn run_core(args: CoreArgs) -> anyhow::Result<()> {
    // `--foreground` is documentation: the core always runs in its own
    // foreground — serve/mcp detach it at spawn time instead.
    let _ = args.foreground;
    // One core per machine: a healthy CURRENT one makes this invocation a
    // no-op; an older one is retired so this binary takes its place.
    retire_older_core();
    if let Some(port) = machine_core() {
        println!("Engram core already running — pane: http://127.0.0.1:{port}");
        return Ok(());
    }
    update::notify_on_newer_release();
    // Test hook: stand in for a first run's model downloads, so the bridge's
    // "still starting" answer can be exercised end to end without them.
    if let Some(ms) = std::env::var("ENGRAM_TEST_CORE_START_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let (hub, models) = build_core_hub(args.fake_embeddings)?;
    let db_display = registry::home_db_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    // The core's process identity + client census + the /shutdown trigger,
    // created before the model wrappers so the idle tracker exists to stamp.
    let (runtime, mut shutdown_rx) = engram_http::CoreRuntime::new(
        registry::engram_home()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    );
    // Every model slot becomes a reloadable lazy handle: idle unload clears
    // them, and any demand reloads single-flight.
    let lazy_models = lazy::LazyModels::install(&models, &hub, &runtime.idle);
    let admin = Arc::new(CortexAdmin {
        models: models.clone(),
        hub: hub.clone(),
        lazy: lazy_models.clone(),
        idle: runtime.idle.clone(),
    });
    let router = engram_http::router_hub_core(
        hub.clone(),
        Some(db_display.clone()),
        admin,
        std::sync::Arc::new(skillgen::SkillInstaller),
        runtime.clone(),
    )
    // `engram-alpha mcp` bridges stdio sessions here, so this core is the
    // one process holding every store.
    .route_service("/mcp", engram_mcp::streamable_http_service(hub.clone()))
    .route("/projects/{id}/mcp", scoped_mcp_route(hub.clone()));
    // Outermost layer, so it sees every request including /mcp. The tracker
    // exempts health, lease and registry paths so polls never keep models warm.
    let stamp_idle = runtime.idle.clone();
    let router = router.layer(axum::middleware::map_request(
        move |req: axum::extract::Request| {
            let idle = stamp_idle.clone();
            async move {
                idle.stamp_http_request(req.uri().path());
                req
            }
        },
    ));
    let preferred = args.http_port.unwrap_or_else(default_http_port);
    let (listener, port) = match bind_or_converge(preferred).await? {
        Bound::Listener(l, port) => (l, port),
        Bound::CoreWonTheRace(port) => {
            println!("Engram core already running — pane: http://127.0.0.1:{port}");
            return Ok(());
        }
    };
    if port != preferred {
        tracing::warn!("port {preferred} was taken; using {port} instead");
    }
    // Record where we landed: `~/.engram/daemon.json` is how every thin
    // client (serve, mcp, doctor, status, stop) finds the core.
    write_machine_daemon_file(port, &db_display);
    tracing::info!("machine core: HTTP + SSE + MCP on http://127.0.0.1:{port}");

    // The watchdog logs any runtime stall past 5 s.
    let watchdog = watchdog::spawn();
    let librarian = spawn_librarian(hub.clone(), runtime.idle.clone());
    let harvester_task = spawn_harvester(hub.clone(), runtime.idle.clone());
    let unload_task = spawn_idle_unloader(runtime.clone(), lazy_models.clone());

    let http = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("http server stopped: {e}");
        }
    });

    // The foreground is the shutdown wait: POST /shutdown, SIGTERM, or
    // ctrl-c all land here and run the same orchestrated exit.
    let reason = wait_for_shutdown(&mut shutdown_rx).await;
    let clients = runtime.clients().len();
    tracing::info!("core shutting down ({reason}) — {clients} connected client(s) released");
    // Background writers first, listener last: /health keeps answering while
    // locks drain, so `stop` never mistakes draining for exiting (issue #8).
    librarian.abort();
    if let Some(task) = &watchdog {
        task.abort();
    }
    harvester_task.abort();
    if let Some(task) = &unload_task {
        task.abort();
    }
    drain_engines(&hub).await;
    remove_repo_daemon_files(port);
    remove_machine_daemon_file();
    // Closing the listener; open sessions/SSE die with the process, which is
    // what tells bridges to exit right away.
    http.abort();
    // Process exit releases every store lock the engines held.
    std::process::exit(0);
}

/// `/projects/{id}/mcp`: a bridge from repo X connects here and its sessions treat
/// X as the current project. One service per project, minted lazily over the hub.
fn scoped_mcp_route(hub: Arc<Hub>) -> axum::routing::MethodRouter {
    let mcp_services: Arc<Mutex<std::collections::HashMap<String, engram_mcp::McpHttpService>>> =
        Arc::default();
    axum::routing::any(
        move |axum::extract::Path(id): axum::extract::Path<String>, req: axum::extract::Request| {
            let services = mcp_services.clone();
            let hub = hub.clone();
            async move {
                use axum::response::IntoResponse;
                let canonical = match hub.resolve_id(&id) {
                    Ok(c) => c,
                    Err(e) => {
                        return (axum::http::StatusCode::NOT_FOUND, e.to_string()).into_response();
                    }
                };
                let mut service = services
                    .locked()
                    .entry(canonical.clone())
                    .or_insert_with(|| {
                        engram_mcp::streamable_http_service_for(hub.clone(), canonical)
                    })
                    .clone();
                match tower::Service::call(&mut service, req).await {
                    Ok(response) => response.into_response(),
                    Err(never) => match never {},
                }
            }
        },
    )
}

/// Every 6 hours (first tick at startup): decay and the conflict scan over every
/// open engine.
fn spawn_librarian(
    sweeper: Arc<Hub>,
    librarian_idle: Arc<engram_http::IdleTracker>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The graph-write count observed after the last COMPLETED pass —
        // snapshotting after the pass absorbs the pass's own writes (decay
        // archives, queued suspects) so they never mark the graph dirty.
        let mut clean_epoch: Option<u64> = None;
        loop {
            tick.tick().await;
            // Skip the sweep while models are unloaded and nothing was written since
            // the last pass: it couldn't find anything new, and would reload the models.
            if librarian_idle.is_unloaded() && clean_epoch == Some(librarian_idle.graph_writes()) {
                tracing::info!(
                    "librarian: models idle-unloaded and nothing changed since the last pass — skipping the sweep"
                );
                continue;
            }
            // Sweep every open engine on the blocking pool: on an async worker it
            // starved bridge pings until every bridge exited (Problem 00fkvifk0v8a).
            let hub = sweeper.clone();
            let started = std::time::Instant::now();
            let swept = tokio::task::spawn_blocking(move || {
                let (mut archived_total, mut added_total) = (0usize, 0usize);
                for engine in hub.engines() {
                    let result = {
                        let decayed = {
                            let mut engine = engine.locked();
                            // Sweep writes (decay archives, scan suspects) are
                            // the daemon's own — attribute them as such in the
                            // journal.
                            engine.set_audit_origin(engram_core::AuditOrigin::daemon());
                            engine.decay(engine.graph_config().policy.decay_ttl_days, false)
                        };
                        // The scan releases the lock per node — a
                        // six-hourly sweep must not freeze the graph it sweeps.
                        decayed.and_then(|archived| {
                            engram_core::Engine::scan_conflicts_shared(&engine)
                                .map(|added| (archived.len(), added))
                        })
                    };
                    match result {
                        Ok((archived, added)) => {
                            archived_total += archived;
                            added_total += added;
                        }
                        Err(e) => tracing::warn!("sweep failed: {e}"),
                    }
                }
                (archived_total, added_total)
            })
            .await;
            match swept {
                Ok((archived_total, added_total)) => tracing::info!(
                    "librarian: sweep took {:.1}s — archived {archived_total} stale nodes, \
                     queued {added_total} suspected conflicts",
                    started.elapsed().as_secs_f64()
                ),
                Err(e) => tracing::warn!("librarian: sweep task panicked: {e}"),
            }
            clean_epoch = Some(librarian_idle.graph_writes());
        }
    })
}

/// Tail assistant transcripts into each project's history layer. The first
/// tick is the backfill; the short interval serves provenance.
fn spawn_harvester(
    harvest_hub: Arc<Hub>,
    harvest_idle: Arc<engram_http::IdleTracker>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = std::env::var("ENGRAM_HARVEST_INTERVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 5)
            .unwrap_or(60);
        let mut harvester = engram_core::harvest::Harvester::first_wave();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // File-system wake-ups pull the next harvest forward. Only an accelerator:
        // the timed sweep stays the guarantee.
        let wake = std::sync::Arc::new(tokio::sync::Notify::new());
        // Held, not just created: dropping a watcher unregisters its watches,
        // so this binding IS the subscription.
        let mut watcher = spawn_transcript_watcher(&harvester, wake.clone());
        if watcher.is_none() {
            tracing::debug!("history watcher not armed — the timed sweep is the only path");
        }
        let mut announced_backfill = false;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = wake.notified() => {
                    // Coalesce the burst a single transcript write produces —
                    // editors and harnesses append in several syscalls, and
                    // one sweep answers all of them.
                    tokio::time::sleep(std::time::Duration::from_millis(
                        WATCH_DEBOUNCE_MS,
                    ))
                    .await;
                    tick.reset();
                }
            }
            let hub = harvest_hub.clone();
            let started = std::time::Instant::now();
            let (h, stats) = tokio::task::spawn_blocking(move || {
                let mut h = harvester;
                let stats = h.sweep(&hub);
                (h, stats)
            })
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("harvest task panicked: {e}");
                (
                    engram_core::harvest::Harvester::first_wave(),
                    Default::default(),
                )
            });
            harvester = h;
            let took = started.elapsed();
            if took >= std::time::Duration::from_secs(5) {
                tracing::info!(
                    "harvest: sweep took {:.1}s (+{} message(s))",
                    took.as_secs_f64(),
                    stats.messages
                );
            }
            if stats.wrote_anything() {
                // Harvest writes dirty the graph for the librarian's
                // idle-unload gate (they don't ride the ChangeEvent path).
                // Their embeddings already extended the model-use clock
                // through the lazy slots, without counting as HTTP activity.
                harvest_idle.note_graph_write();
                if !announced_backfill {
                    announced_backfill = true;
                    tracing::info!(
                        "history backfill: {} message(s) across {} new session(s) ingested",
                        stats.messages,
                        stats.sessions
                    );
                } else {
                    tracing::debug!(
                        "harvest: +{} message(s), {} new session(s)",
                        stats.messages,
                        stats.sessions
                    );
                }
                // New sessions can mean new transcript directories. Re-arming
                // is how the watcher learns about them; until it does, the
                // timed sweep is what covers them.
                if stats.sessions > 0 {
                    watcher = spawn_transcript_watcher(&harvester, wake.clone());
                    tracing::trace!("history watcher re-armed: {}", watcher.is_some());
                }
            }
        }
    })
}

/// After [`idle_unload_secs`] with no lease, no counted HTTP activity and no
/// model use, drop the ONNX sessions; the next demand reloads them.
fn spawn_idle_unloader(
    runtime: Arc<engram_http::CoreRuntime>,
    lazy_models: lazy::LazyModels,
) -> Option<tokio::task::JoinHandle<()>> {
    let idle_secs = idle_unload_secs();
    (idle_secs > 0).then(|| {
        tokio::spawn(async move {
            let idle = runtime.idle.clone();
            // Sample often enough that short test windows work, rarely
            // enough to be free at the default 15m.
            let period = (idle_secs / 10).clamp(1, 30);
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(period));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                // A connected-but-idle bridge keeps models resident by
                // design; observing a live lease re-arms the lease clock
                // (renewals stamp it too, between samples).
                if !runtime.clients().is_empty() {
                    idle.touch_lease();
                    continue;
                }
                if idle.is_unloaded() {
                    continue;
                }
                let now = engram_core::now();
                let quiet_for = idle_secs as i64;
                let all_quiet = [
                    idle.last_http_activity(),
                    idle.last_model_use(),
                    idle.last_lease_seen(),
                ]
                .into_iter()
                .all(|t| now.saturating_sub(t) >= quiet_for);
                if !all_quiet {
                    continue;
                }
                let rss_before = process_rss_mb();
                if lazy_models.unload_all() {
                    idle.mark_unloaded(now);
                    // Honest accounting, not a promise (the 0.8.3 lesson):
                    // report what the allocator actually gave back.
                    match (rss_before, process_rss_mb()) {
                        (Some(before), Some(after)) => tracing::info!(
                            "models unloaded after {} idle (freed ~{} MB: RSS {before} MB → {after} MB)",
                            humane_duration(quiet_for),
                            before.saturating_sub(after),
                        ),
                        _ => tracing::info!(
                            "models unloaded after {} idle",
                            humane_duration(quiet_for)
                        ),
                    }
                }
            }
        })
    })
}

/// Drain in-flight engine work by taking each lock once, bounded at 10 s; both
/// backends are crash-safe if a wedged job outlives the deadline. A poisoned
/// lock is free, not busy.
async fn drain_engines(hub: &Hub) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    for engine in hub.engines() {
        while matches!(engine.try_lock(), Err(std::sync::TryLockError::WouldBlock)) {
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    "an engine was still busy at the drain deadline — exiting anyway (crash-safe)"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

/// Resolve `/shutdown` (the watch), SIGTERM, and ctrl-c into one exit reason.
pub(crate) async fn wait_for_shutdown(rx: &mut tokio::sync::watch::Receiver<bool>) -> &'static str {
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = rx.changed() => "POST /shutdown",
        _ = tokio::signal::ctrl_c() => "ctrl-c",
        _ = term => "SIGTERM",
    }
}

/// Remove the repo-local `daemon.json` advertisements that point at this
/// core — `serve` refreshed them with our port; a file left behind would be
/// a stale pointer every reader has to health-check around.
pub(crate) fn remove_repo_daemon_files(port: u16) {
    for entry in registry::load().projects {
        let Some(dir) = Path::new(&entry.db).parent() else {
            continue;
        };
        let file = dir.join("daemon.json");
        let advertised = std::fs::read_to_string(&file)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v["port"].as_u64());
        if advertised == Some(u64::from(port)) {
            let _ = std::fs::remove_file(&file);
        }
    }
}

/// `ENGRAM_IDLE_UNLOAD_SECS` (default 900, 0 disables): see the idle unload in
/// [`run_core`]. Read from env so an auto-spawned core honours it.
pub(crate) fn idle_unload_secs() -> u64 {
    std::env::var("ENGRAM_IDLE_UNLOAD_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900)
}

/// This process's resident set in MB (`ps`, works on macOS and Linux) —
/// sampled only around an unload to put honest numbers in the log line.
pub(crate) fn process_rss_mb() -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb / 1024)
}

/// The core's default port: `$ENGRAM_HTTP_PORT` (sandboxes and tests), else
/// 8787 — kept out of clap so an auto-spawned core (no flags) honors it too.
pub(crate) fn default_http_port() -> u16 {
    std::env::var("ENGRAM_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8787)
}

/// How long a filesystem wake-up waits before sweeping, so one transcript
/// write — which arrives as several syscalls — costs one sweep, not several.
pub(crate) const WATCH_DEBOUNCE_MS: u64 = 400;

/// Poke `wake` when a watched transcript directory changes. Every failure is
/// soft: the timed sweep still runs. Watches non-recursively, since the roots
/// are the exact parents of discovered transcripts.
pub(crate) fn spawn_transcript_watcher(
    harvester: &engram_core::harvest::Harvester,
    wake: std::sync::Arc<tokio::sync::Notify>,
) -> Option<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};

    let roots = harvester.watch_roots();
    if roots.is_empty() {
        return None;
    }
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // `notify_one` stores a permit when nobody is waiting, so a write that
        // lands mid-sweep still earns the next one — no event is dropped for
        // arriving at a busy moment.
        if let Ok(event) = res
            && matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
        {
            wake.notify_one();
        }
    })
    .inspect_err(|e| tracing::debug!("history watcher unavailable: {e}"))
    .ok()?;

    let mut watched = 0usize;
    for root in &roots {
        match watcher.watch(root, RecursiveMode::NonRecursive) {
            Ok(()) => watched += 1,
            Err(e) => tracing::debug!("history watcher skipped {}: {e}", root.display()),
        }
    }
    if watched == 0 {
        return None;
    }
    tracing::debug!("history watcher armed on {watched} transcript directories");
    Some(watcher)
}

pub(crate) enum Bound {
    Listener(tokio::net::TcpListener, u16),
    /// The port is held by another engram daemon — two serves raced and the
    /// other one won; this invocation converges instead of walking ports.
    CoreWonTheRace(u16),
}

/// Bind the requested port or walk forward. If this home's core already
/// answers /health on a taken port, join it instead (one core, one pane); a
/// foreign daemon is just a taken port.
pub(crate) async fn bind_or_converge(start: u16) -> anyhow::Result<Bound> {
    const TRIES: u16 = 16;
    for offset in 0..TRIES {
        let port = start + offset;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => return Ok(Bound::Listener(l, port)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Three probes, not one: a busy-but-alive core failing a
                // single 3s read would read as "foreign", and walking past
                // it opens a second core over the same home graph.
                let mut health = None;
                for attempt in 0..3 {
                    health = doctor::http_get(port, "/health");
                    if health.is_some() {
                        break;
                    }
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
                let parsed = health
                    .as_deref()
                    .and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok());
                let ours = parsed
                    .as_ref()
                    .and_then(|v| v["db"].as_str().map(str::to_string))
                    .zip(registry::home_db_path())
                    .is_some_and(|(db, home)| Path::new(&db) == home);
                if ours {
                    let version = parsed
                        .as_ref()
                        .and_then(|v| v["version"].as_str().map(str::to_string));
                    // Version handshake at the bind: an older core holding
                    // our port yields to this binary — orchestrated stop,
                    // wait for its actual exit (pid, not just health: the
                    // locks release at process exit), then take the port.
                    if core_is_older(version.as_deref()) {
                        tracing::info!(
                            "core v{} holds port {port} — this binary is v{}; taking over",
                            version.as_deref().unwrap_or("unknown"),
                            engram_core::advertised_version()
                        );
                        let pid = machine_core_pid();
                        if doctor::http_post(port, "/shutdown", "{}").is_some() {
                            wait_core_exit(pid, port, 60);
                        }
                        if let Ok(l) = tokio::net::TcpListener::bind(addr).await {
                            return Ok(Bound::Listener(l, port));
                        }
                        tracing::warn!(
                            "the older core didn't release port {port} — converging on it"
                        );
                    }
                    return Ok(Bound::CoreWonTheRace(port));
                }
                if health.is_none() {
                    tracing::warn!(
                        "port {port} is taken and won't answer /health — treating it as foreign \
                         and walking on (if this is a wedged engram core, `engram-alpha stop` it)"
                    );
                }
            }
            Err(e) => return Err(e).with_context(|| format!("binding {addr}")),
        }
    }
    anyhow::bail!("no free port in {start}..{}", start + TRIES)
}
