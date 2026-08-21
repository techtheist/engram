//! The engram-alpha CLI. Process model (0.8.8): ONE machine core per user —
//! `engram-alpha core`, spawned detached and holding every store lock, the
//! models, the pane, and the MCP endpoints — plus N truly-light access
//! points: `serve` ensures the core and registers the project, `mcp` bridges
//! a stdio client to it, `status`/`stop` observe and end it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::{Parser, Subcommand};
use engram_core::{
    Embedder, Engine, ExportGraph, FakeEmbedder, FastEmbedder, Hub, Nli, Reranker, registry,
};
use tracing_subscriber::EnvFilter;

mod doctor;
mod lazy;
mod setup;
mod skillgen;
mod update;

#[derive(Parser)]
#[command(
    name = "engram-alpha",
    version,
    about = "Durable graph memory for AI coding assistants"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve this repo's memory and print the pane URL.
    ///
    /// Makes sure the machine core is running (starting it detached if
    /// needed), registers this repo's graph with it, prints the pane URL,
    /// and exits. The core keeps serving after this command returns; safe
    /// to run from anywhere, as many times as you like.
    Serve(ServeArgs),
    /// Stdio MCP bridge — what your MCP client launches, not you.
    ///
    /// A light proxy to the machine core's MCP endpoint. Never opens the
    /// store itself; starts the core when the machine has none. Without
    /// --db it binds to the project the client's MCP roots name (cwd as the
    /// fallback) and follows the client across project switches.
    Mcp(McpArgs),
    /// Internal: run the machine core in the foreground.
    ///
    /// The one process holding every store, the models, the pane, and the
    /// MCP endpoints. `serve` and `mcp` start it detached; running it by
    /// hand keeps it in the foreground.
    #[command(hide = true)]
    Core(CoreArgs),
    /// Show what's running: core, models, projects, connected clients.
    ///
    /// Read live from the core's API; `--json` for scripts.
    Status(StatusArgs),
    /// Export the whole graph as portable JSON (to a file or stdout).
    Export(ExportArgs),
    /// Import a JSON snapshot (upsert by id; idempotent).
    Import(ImportArgs),
    /// Print the session-start brief for this repo's graph.
    ///
    /// A compact markdown digest of the graph's canon: conflicts, open
    /// work, principles, decisions, cautions.
    Brief(BriefArgs),
    /// Update this binary to the latest release.
    ///
    /// Downloads the release for this platform, verifies its checksum, and
    /// swaps the binary in place. A no-op when already current.
    Update(UpdateArgs),
    /// Check this repo's Engram installation and say what to fix.
    ///
    /// Covers store integrity (asking the machine core when it holds the
    /// store), core health, the embedding model, update availability, and
    /// every detected assistant's wiring. Exits non-zero when something
    /// needs fixing, so it doubles as a pre-flight in scripts.
    Doctor(DoctorArgs),
    /// Wire the current repository for AI assistants.
    ///
    /// MCP registration plus capture instructions, from assets embedded in
    /// this binary. With no --cli, auto-detects which assistants are
    /// installed and wires those. Idempotent — re-running never duplicates.
    Setup(SetupArgs),
    /// Move this repo's graph onto the TepinDB backend.
    ///
    /// Nodes and edges travel as the canonical JSON export (embeddings
    /// regenerated); the suspect queue and audit journal are carried over
    /// verbatim; the old graph.db stays behind untouched as a backup.
    /// Every command picks up graph.tepin automatically afterwards.
    Migrate(MigrateArgs),
    /// Stop the machine core — every project, one gesture.
    ///
    /// An orchestrated shutdown over the core's API (sessions closed, every
    /// store lock released, daemon files removed), with a PID-kill fallback
    /// when it doesn't answer. Bridges exit on their own when the core goes.
    Stop,
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Path to the graph database to register with the core (created on
    /// first access).
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Port the core should listen on when this launch has to start it
    /// (127.0.0.1 only; walks forward when taken).
    #[arg(long)]
    http_port: Option<u16>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    ///
    /// For offline use and quick smoke tests — fake vectors are noise for
    /// real searches. Propagated to the core when this launch starts it.
    #[arg(long)]
    fake_embeddings: bool,
    /// Deprecated: stay in the foreground while the core is healthy.
    ///
    /// Pre-0.8.8 shape kept for process supervisors: serve now always
    /// ensures the shared machine core; with this flag it blocks while the
    /// core is healthy and exits when the core dies.
    #[arg(long)]
    http_only: bool,
}

#[derive(clap::Args)]
struct CoreArgs {
    /// Port for the HTTP API + pane (bound to 127.0.0.1; walks forward when
    /// taken). Default: $ENGRAM_HTTP_PORT, else 8787.
    #[arg(long)]
    http_port: Option<u16>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    fake_embeddings: bool,
    /// No-op: the core always runs in the foreground of its own process.
    ///
    /// `serve` and `mcp` detach it for you; this flag exists for humans
    /// starting a core by hand and changes nothing.
    #[arg(long)]
    foreground: bool,
}

#[derive(clap::Args)]
struct StatusArgs {
    /// Machine-readable output for scripts.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct McpArgs {
    /// Pin this session to one graph database (created if missing).
    ///
    /// Optional since 0.8.8: without it the session binds to a project by
    /// the MCP client's roots (first file:// root; the bridge's working
    /// directory when the client has none) and follows roots/list_changed
    /// across project switches — one global config entry serves every
    /// project. When neither names a usable project (an IDE launch from `/`
    /// whose client never answers roots/list), the session binds the
    /// "default agent project" from the pane's Settings, falling to the
    /// home graph when unset. An explicit --db pins the project and
    /// disables roots binding.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct ExportArgs {
    /// Path to the graph database to export.
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Write to this file instead of stdout.
    #[arg(long, short)]
    out: Option<PathBuf>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct ImportArgs {
    /// Path to the graph database to import into (created if missing).
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// JSON snapshot file to import.
    file: PathBuf,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    ///
    /// Tests only — vectors written this way are noise for real searches.
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct BriefArgs {
    /// Path to the graph database to brief from.
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Character budget for the digest (default: the graph's configured
    /// brief budget).
    #[arg(long)]
    max_chars: Option<usize>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct UpdateArgs {
    /// Release tag to install (e.g. v0.8.7). Default: the latest release.
    #[arg(long)]
    version: Option<String>,
}

#[derive(clap::Args)]
struct DoctorArgs {
    /// Path to the graph database this repo is expected to use.
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
}

#[derive(clap::Args)]
struct MigrateArgs {
    /// The SQLite graph to migrate (the .tepin file lands next to it).
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Rebuild an existing graph.tepin target instead of refusing.
    #[arg(long)]
    force: bool,
    /// Use the deterministic fake embedder (tests only — the migrated graph's
    /// vectors would be noise for real searches).
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct SetupArgs {
    /// Assistants to wire, comma-separated: claude|codex|gemini|opencode|kilo|antigravity|bob|windsurf|all.
    ///
    /// Default: auto-detect what's installed. windsurf writes both global
    /// configs — ${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json and
    /// ~/.devin/mcp_config.json (db-less — the bridge binds by MCP roots);
    /// the rest wire this repository.
    #[arg(long)]
    cli: Option<String>,
    /// Capture intensity for the installed instructions/skill.
    #[arg(long, default_value = "relaxed", value_parser = ["relaxed", "normal", "aggressive"])]
    skill: String,
    /// Only register the MCP server (and git-ignore the graph) — skip the
    /// skill, instruction blocks, and hooks. For setups where something else
    /// already ships those, like the Claude Code plugin.
    #[arg(long)]
    mcp_only: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.command);

    match cli.command {
        Command::Serve(args) => serve(args).await,
        Command::Core(args) => run_core(args).await,
        Command::Status(args) => run_status(args),
        Command::Mcp(args) => {
            let result = run_mcp(args).await;
            let code = match &result {
                Ok(()) => 0,
                Err(e) => {
                    // The bridge's fatal error must reach mcp.log — IDE
                    // clients swallow stderr, and this line is the whole
                    // diagnosis.
                    tracing::error!("mcp exiting: {e:#}");
                    1
                }
            };
            // Exit WITHOUT dropping the tokio runtime: tokio's stdin driver
            // parks a blocking-pool thread in a read() that only returns
            // when the client writes or closes stdin, and Runtime::drop
            // waits for that thread — an MCP client that keeps stdin open
            // while awaiting a response deadlocks us into a zombie
            // Nothing here needs destructors:
            // the census lease is already withdrawn and the log writers are
            // unbuffered.
            std::process::exit(code);
        }
        Command::Export(args) => run_export(args),
        Command::Import(args) => run_import(args),
        Command::Brief(args) => run_brief(args),
        Command::Update(args) => run_update(args),
        Command::Doctor(args) => doctor::run(&engram_core::resolve_db_path(&args.db)),
        Command::Setup(args) => run_setup(args),
        Command::Migrate(args) => run_migrate(args),
        Command::Stop => run_stop(),
    }
}

/// Logs go to STDERR: stdout is the MCP protocol channel and must stay clean.
/// The `mcp` command additionally tees into `.engram/mcp.log` next to the
/// store (append; `ENGRAM_MCP_LOG=0` disables) — IDE clients swallow stderr,
/// which left bridge failures like a proxy-intercepted loopback invisible.
/// RUST_LOG controls the level of both writers.
fn init_tracing(command: &Command) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{Layer, fmt};

    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = match command {
        Command::Mcp(args) if std::env::var("ENGRAM_MCP_LOG").as_deref() != Ok("0") => {
            // db-less bridges tee into the cwd's .engram (Roots binding may
            // move the session later, but the log needs a home up front).
            let db = args.db.clone().unwrap_or_else(default_db_path);
            mcp_log_file(&db).map(|f| {
                fmt::layer()
                    .with_writer(f)
                    .with_ansi(false)
                    .with_filter(filter())
            })
        }
        _ => None,
    };
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter()),
        )
        .with(file_layer)
        .init();
}

fn mcp_log_file(db: &Path) -> Option<std::sync::Arc<std::fs::File>> {
    let open = |dir: &Path| {
        std::fs::create_dir_all(dir).ok()?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("mcp.log"))
            .ok()
    };
    let dir = engram_core::resolve_db_path(db).parent()?.to_path_buf();
    open(&dir)
        // An IDE-spawned bridge can land in an unwritable cwd (/, a vanished
        // dir) — tee into the machine-level ~/.engram instead of losing the
        // only forensics an IDE client leaves us (the 0.8.5 lesson: mcp.log
        // sees what IDE clients swallow).
        .or_else(|| open(&registry::engram_home()?))
        .map(std::sync::Arc::new)
}

/// Orchestrated stop: ask the machine core to shut itself down over
/// `POST /shutdown` (sessions closed, engines dropped, every store lock
/// released, daemon files removed), wait for it to actually exit, and only
/// fall back to a PID kill when it doesn't answer or the file is stale.
/// Legacy per-repo daemons (pre-0.8.8 binaries) still get the old
/// health-verified kill.
fn run_stop() -> anyhow::Result<()> {
    let mut stopped = 0;
    if let Some(home) = registry::engram_home() {
        stopped += stop_machine_core(&home.join("daemon.json"));
    }

    // Legacy per-repo daemons, discovered from the daemon files we already
    // keep (never by process-name matching), health-verified before each
    // kill so a stale file can't take down an unrelated reused pid. Files
    // that advertised the now-stopped core clean themselves up here too.
    for entry in registry::load().projects {
        let Some(dir) = Path::new(&entry.db).parent() else {
            continue;
        };
        let file = dir.join("daemon.json");
        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
            let _ = std::fs::remove_file(&file);
            continue;
        };
        let (Some(port), Some(pid)) = (meta["port"].as_u64(), meta["pid"].as_u64()) else {
            continue;
        };
        if doctor::http_get(port as u16, "/health").is_none() {
            // Nothing lives there — the file is stale; clean it up.
            let _ = std::fs::remove_file(&file);
            continue;
        }
        let ok = terminate_pid(pid as u32);
        if ok {
            // Give it a moment, then confirm the port went quiet.
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if doctor::http_get(port as u16, "/health").is_none() {
                    break;
                }
            }
            let _ = std::fs::remove_file(&file);
            println!("stopped {} (pid {pid}, port {port})", entry.name);
            stopped += 1;
        } else {
            eprintln!(
                "couldn't stop {} (pid {pid}) — stop it manually",
                entry.name
            );
        }
    }
    if stopped == 0 {
        println!("nothing to stop — no healthy engram daemons advertised");
    }
    Ok(())
}

/// Stop the machine core advertised in `file`. Returns how many processes
/// were stopped (0 or 1).
fn stop_machine_core(file: &Path) -> usize {
    let Ok(raw) = std::fs::read_to_string(file) else {
        return 0;
    };
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
        let _ = std::fs::remove_file(file);
        return 0;
    };
    let Some(port) = meta["port"].as_u64().map(|p| p as u16) else {
        let _ = std::fs::remove_file(file);
        return 0;
    };
    if doctor::http_get(port, "/health").is_none() {
        let _ = std::fs::remove_file(file);
        return 0;
    }
    // What is about to be cut loose — reported after the stop.
    let clients = doctor::http_get(port, "/system")
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|v| v["processes"]["clients"].as_array().map(|c| c.len()));
    let pid = meta["pid"].as_u64();

    // Orchestrated first: the core closes sessions, drops every engine (all
    // store locks released), removes its daemon files, and exits 0.
    let mut gone = false;
    if doctor::http_post(port, "/shutdown", "{}").is_some() {
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if doctor::http_get(port, "/health").is_none() {
                gone = true;
                break;
            }
        }
    }
    if !gone {
        // Unresponsive, or an older binary without /shutdown: PID kill.
        if let Some(pid) = pid
            && terminate_pid(pid as u32)
        {
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if doctor::http_get(port, "/health").is_none() {
                    gone = true;
                    break;
                }
            }
        }
    }
    if !gone {
        eprintln!(
            "couldn't stop the machine core (pid {}, port {port}) — stop it manually",
            pid.unwrap_or_default()
        );
        return 0;
    }
    let _ = std::fs::remove_file(file); // the core removes it itself; belt and braces
    match clients {
        Some(n) => println!(
            "stopped machine core (pid {}, port {port}) — {n} connected client(s) released",
            pid.unwrap_or_default()
        ),
        None => println!(
            "stopped machine core (pid {}, port {port})",
            pid.unwrap_or_default()
        ),
    }
    1
}

fn terminate_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg(pid.to_string())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// `engram-alpha status` (issue #5): a thin client of the core's GET /system
/// and GET /projects — core identity, model residency, registered projects
/// with their lock state, and every connected client, in humane terms.
fn run_status(args: StatusArgs) -> anyhow::Result<()> {
    let Some(port) = machine_core() else {
        if args.json {
            println!("{}", serde_json::json!({ "running": false }));
        } else {
            println!("engram core: not running");
            println!(
                "  start it with `engram-alpha serve` (from a repo) or let an MCP client spawn it"
            );
        }
        return Ok(());
    };
    let system = doctor::http_get(port, "/system")
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .context("the core answered /health but not /system")?;
    let projects = doctor::http_get(port, "/projects")
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .unwrap_or_else(|| serde_json::json!([]));

    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "running": true,
                "port": port,
                "system": system,
                "projects": projects,
            })
        );
        return Ok(());
    }

    let now = engram_core::now();
    let core = &system["processes"]["core"];
    let uptime = system["daemon"]["uptime_secs"].as_i64().unwrap_or(0);
    println!("engram core");
    println!(
        "  pid {} · v{} · up {} · port {port}",
        core["pid"].as_u64().unwrap_or(0),
        core["version"]
            .as_str()
            .or(system["version"].as_str())
            .unwrap_or("?"),
        humane_duration(uptime),
    );
    if let Some(home) = core["home"].as_str() {
        println!("  home {home}");
    }
    let models = &system["models_state"];
    match (models["state"].as_str(), models["since"].as_i64()) {
        (Some("unloaded_idle"), Some(since)) => println!(
            "  models: unloaded (idle since {})",
            humane_ago(now.saturating_sub(since))
        ),
        (Some(state), Some(since)) => println!(
            "  models: {state} (since {})",
            humane_ago(now.saturating_sub(since))
        ),
        _ => println!(
            "  models: {}",
            if system["model_cached"].as_bool() == Some(true) {
                "cached"
            } else {
                "not downloaded"
            }
        ),
    }

    println!("\nprojects");
    match projects.as_array().filter(|p| !p.is_empty()) {
        Some(list) => {
            for p in list {
                let name = p["name"].as_str().unwrap_or("?");
                let place = p["root"].as_str().or(p["db"].as_str()).unwrap_or("?");
                let state = if p["open"].as_bool() == Some(true) {
                    "store open (held by the core)"
                } else {
                    "registered"
                };
                println!("  {name} · {place} · {state}");
            }
        }
        None => println!("  (none registered)"),
    }

    println!("\nclients");
    match system["processes"]["clients"]
        .as_array()
        .filter(|c| !c.is_empty())
    {
        Some(clients) => {
            for c in clients {
                // "mcp-bridge (mcp-go)" — which client bound where, and via
                // what. Older bridges never sent a name; the bare kind stays.
                let kind = match c["client"].as_str() {
                    Some(name) => format!("{} ({name})", c["kind"].as_str().unwrap_or("?")),
                    None => c["kind"].as_str().unwrap_or("?").to_string(),
                };
                println!(
                    "  {kind} · pid {} · {} · connected {} · seen {}",
                    c["pid"].as_u64().unwrap_or(0),
                    c["root"].as_str().unwrap_or("?"),
                    humane_ago(now.saturating_sub(c["connected_at"].as_i64().unwrap_or(now))),
                    humane_ago(now.saturating_sub(c["last_seen"].as_i64().unwrap_or(now))),
                );
            }
        }
        None => println!("  (none connected)"),
    }
    Ok(())
}

/// `7381` → `"2h 3m"` — two units at most, seconds only under a minute.
fn humane_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m, s) = (
        secs / 86_400,
        (secs % 86_400) / 3_600,
        (secs % 3_600) / 60,
        secs % 60,
    );
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, m) => format!("{m}m {s}s"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

fn humane_ago(secs: i64) -> String {
    if secs <= 0 {
        "just now".into()
    } else {
        format!("{} ago", humane_duration(secs))
    }
}

/// The §7C step-5 cutover, per repo: JSON export/import is the vehicle for
/// nodes + edges (embeddings regenerated on the way in), suspects and the
/// audit journal ride over verbatim, counts are verified, and the registry
/// entry repoints — the SQLite source is never modified.
fn run_migrate(args: MigrateArgs) -> anyhow::Result<()> {
    let src_path = args.db;
    anyhow::ensure!(
        src_path.exists(),
        "{} does not exist — nothing to migrate",
        src_path.display()
    );
    anyhow::ensure!(
        !engram_core::is_tepin_path(&src_path),
        "{} is already a TepinDB store",
        src_path.display()
    );
    let dst_path = src_path.with_extension("tepin");
    if dst_path.exists() {
        anyhow::ensure!(
            args.force,
            "{} already exists — pass --force to rebuild it from the SQLite source",
            dst_path.display()
        );
        std::fs::remove_file(&dst_path)
            .with_context(|| format!("removing {}", dst_path.display()))?;
    }

    let models = load_models(args.fake_embeddings, false)?;
    let summary = engram_core::migrate_to_tepin(
        &src_path,
        models.current().embedder,
        engram_core::AuditOrigin::cli(),
    )
    .with_context(|| format!("migrating {}", src_path.display()))?;

    // Repoint this repo's registry entry so every hub opens the new store.
    if let Some(root) = dst_path
        .canonicalize()
        .ok()
        .and_then(|p| {
            let dir = p.parent()?;
            (dir.file_name()? == ".engram").then(|| dir.parent().map(Path::to_path_buf))?
        })
        .or_else(|| std::env::current_dir().ok())
        && let Err(e) = registry::register(&root, &dst_path)
    {
        eprintln!("note: couldn't repoint ~/.engram/registry.json: {e}");
    }

    println!(
        "migrated {} nodes / {} edges / {} suspects / {} audit rows to {}",
        summary.nodes,
        summary.edges,
        summary.suspects,
        summary.audit,
        dst_path.display()
    );
    println!(
        "{} is untouched (your backup); every engram-alpha command now picks {} automatically — restart the daemon and reconnect MCP",
        src_path.display(),
        dst_path.file_name().unwrap_or_default().display()
    );
    Ok(())
}

fn run_setup(args: SetupArgs) -> anyhow::Result<()> {
    let agents: Vec<&str> = match args.cli.as_deref() {
        Some("all") => setup::AGENTS.to_vec(),
        Some(list) => {
            let mut v = Vec::new();
            for a in list.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                let known = setup::AGENTS.iter().find(|k| **k == a).with_context(|| {
                    format!(
                        "unknown --cli '{a}' (claude|codex|gemini|opencode|kilo|antigravity|bob|windsurf|all)"
                    )
                })?;
                v.push(*known);
            }
            v
        }
        None => {
            let detected = setup::detect_agents();
            anyhow::ensure!(
                !detected.is_empty(),
                "no supported assistants detected — pick explicitly with --cli"
            );
            println!("==> detected: {}", detected.join(", "));
            detected
        }
    };
    anyhow::ensure!(!agents.is_empty(), "nothing to wire");
    setup::Setup::new(&args.skill, args.mcp_only)?.run(&agents)?;
    // Wiring a repo makes it a project — put it on the machine registry so
    // every other project's hub can see it (PLAN §7C). Best-effort.
    if let Ok(cwd) = std::env::current_dir()
        && let Err(e) = registry::register(&cwd, &cwd.join(".engram/graph.db"))
    {
        eprintln!("note: couldn't add this repo to ~/.engram/registry.json: {e}");
    }
    Ok(())
}

/// Self-update from GitHub Releases: download the platform asset, verify its
/// published sha256, and atomically swap the running binary. Uses the system
/// curl/tar (present on macOS, Linux, WSL, and Windows 10+) so the binary
/// doesn't carry an HTTP client for one command.
fn run_update(args: UpdateArgs) -> anyhow::Result<()> {
    let (target, ext, bin_name) = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "tar.gz", "engram-alpha")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ("x86_64-unknown-linux-gnu", "tar.gz", "engram-alpha")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", "exe", "engram-alpha.exe")
    } else {
        anyhow::bail!("no prebuilt binaries for this platform — update from source instead");
    };

    let tag = match args.version {
        Some(v) => v,
        None => update::latest_release_tag()?,
    };

    let asset = format!("engram-alpha-{tag}-{target}.{ext}");
    let url = format!(
        "https://github.com/{}/releases/download/{tag}/{asset}",
        update::REPO
    );
    let tmp = std::env::temp_dir().join(format!("engram-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let cleanup = scopeguard(tmp.clone());
    let archive = tmp.join(&asset);

    eprintln!("downloading {asset}…");
    let fetch = |from: &str, to: &Path| -> anyhow::Result<()> {
        let status = std::process::Command::new("curl")
            .args(["-fL", "--progress-bar", "-o"])
            .arg(to)
            .arg(from)
            .status()
            .context("running curl")?;
        anyhow::ensure!(status.success(), "download failed: {from}");
        Ok(())
    };
    fetch(&url, &archive)?;
    let sums = tmp.join(format!("{asset}.sha256"));
    fetch(&format!("{url}.sha256"), &sums)?;

    let expected = std::fs::read_to_string(&sums)?
        .split_whitespace()
        .next()
        .map(str::to_lowercase)
        .context("empty checksum file")?;
    anyhow::ensure!(
        sha256_file(&archive)? == expected,
        "checksum mismatch — refusing to install"
    );

    let new_bin = if ext == "tar.gz" {
        let status = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&tmp)
            .status()
            .context("running tar")?;
        anyhow::ensure!(status.success(), "extracting {asset} failed");
        tmp.join(bin_name)
    } else {
        archive
    };

    let exe = std::env::current_exe().context("locating the running binary")?;
    if sha256_file(&new_bin)? == sha256_file(&exe)? {
        println!("already up to date ({tag})");
        drop(cleanup);
        return Ok(());
    }

    // Stage next to the target so the final rename is atomic (same filesystem).
    let staged = exe.with_extension("new");
    std::fs::copy(&new_bin, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    {
        // Windows can't replace a running exe, but it can rename it away.
        let old = exe.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old)?;
    }
    std::fs::rename(&staged, &exe)?;
    println!(
        "updated {} to {tag} — restart your daemon (engram-alpha serve)",
        exe.display()
    );
    drop(cleanup);
    Ok(())
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Remove the temp dir when dropped, however the update exits.
fn scopeguard(dir: PathBuf) -> impl Drop {
    struct G(PathBuf);
    impl Drop for G {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    G(dir)
}

/// Resolve the owning daemon's route for one of this store's endpoints:
/// `{suffix}` on a repo-local daemon, `/projects/{id}{suffix}` on the machine
/// core (registering the repo on the way). `None` = no daemon owns the store
/// and the caller may open the file directly.
fn daemon_route(db: &Path, suffix: &str) -> Option<(u16, String)> {
    let url = resolve_mcp_target(db)?.replace("/mcp", suffix);
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let (port, path) = rest.split_once('/')?;
    Some((port.parse().ok()?, format!("/{path}")))
}

fn run_brief(args: BriefArgs) -> anyhow::Result<()> {
    // Thin client first: a running daemon owns the store (and on TepinDB is
    // the only process allowed to). A repo-launched daemon serves this
    // project at /brief; the machine core serves it at the scoped route.
    let db = engram_core::resolve_db_path(&args.db);
    // No explicit budget → the owning graph's configured one (daemon-side
    // when bridged, config read when opening directly).
    let suffix = match args.max_chars {
        Some(n) => format!("/brief?max_chars={n}"),
        None => "/brief".to_string(),
    };
    if let Some((port, path)) = daemon_route(&db, &suffix)
        && let Some(text) = doctor::http_get(port, &path)
    {
        println!("{}", text.trim_end());
        return Ok(());
    }
    // The hub form appends the home-graph section (PLAN §7C); a brief is a
    // read, so it deliberately does not touch the registry.
    let (hub, _) = build_hub(&db, args.fake_embeddings, false)?;
    let max_chars = hub
        .current_engine()
        .lock()
        .unwrap()
        .brief_chars(args.max_chars);
    println!("{}", hub.brief(max_chars)?);
    Ok(())
}

fn run_export(args: ExportArgs) -> anyhow::Result<()> {
    // Thin client first (PLAN §7D): a daemon owning this store serves
    // GET /export — a direct open would trip the tepin single-owner lock.
    let db = engram_core::resolve_db_path(&args.db);
    let snapshot: ExportGraph = if let Some((port, path)) = daemon_route(&db, "/export")
        && let Some(body) = doctor::http_get(port, &path)
    {
        serde_json::from_str(&body).context("parsing the daemon's export")?
    } else {
        build_engine(&db, args.fake_embeddings, false)?.export()?
    };
    let json = serde_json::to_string_pretty(&snapshot)?;
    match args.out {
        Some(path) => {
            std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(
                "exported {} nodes / {} edges to {}",
                snapshot.nodes.len(),
                snapshot.edges.len(),
                path.display()
            );
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn run_import(args: ImportArgs) -> anyhow::Result<()> {
    let data = std::fs::read_to_string(&args.file)
        .with_context(|| format!("reading {}", args.file.display()))?;
    // Parse locally first — a malformed file fails here, not at the daemon.
    let graph: ExportGraph = serde_json::from_str(&data).context("parsing export JSON")?;
    let db = engram_core::resolve_db_path(&args.db);
    // Thin client first: the owning daemon runs the import (re-embedding
    // takes a while — generous timeout), which also feeds the pane's SSE.
    let (nodes, edges) = if let Some((port, path)) = daemon_route(&db, "/import") {
        let resp =
            doctor::http_post_timeout(port, &path, &data, std::time::Duration::from_secs(600))
                .context("the daemon rejected the import — check its log")?;
        let v: serde_json::Value =
            serde_json::from_str(&resp).context("parsing the daemon's import summary")?;
        (
            v["nodes"].as_u64().unwrap_or_default() as usize,
            v["edges"].as_u64().unwrap_or_default() as usize,
        )
    } else {
        let mut engine = build_engine(&db, args.fake_embeddings, false)?;
        engine.set_audit_origin(engram_core::AuditOrigin::cli());
        let summary = engine.import(graph)?;
        (summary.nodes, summary.edges)
    };
    tracing::info!(
        "imported {nodes} nodes / {edges} edges into {}",
        db.display()
    );
    Ok(())
}

/// One loaded model set — what every engine the process opens is wired to
/// (PLAN §7C: the hub loads the cortex once, not once per project).
#[derive(Clone)]
struct ModelSet {
    embedder: Arc<dyn Embedder>,
    reranker: Option<Arc<dyn Reranker>>,
    nli: Option<Arc<dyn Nli>>,
}

/// The live model runtime. The `RwLock` is model selection's hot-swap point
/// (PLAN §7A): `/models` replaces a slot, already-open engines get the new
/// handle pushed in, engines opened later read the current set here.
#[derive(Clone)]
struct Models {
    set: Arc<std::sync::RwLock<ModelSet>>,
}

impl Models {
    fn current(&self) -> ModelSet {
        self.set.read().expect("model set lock").clone()
    }
}

/// Load the models the machine-level selection (`~/.engram/models.json`)
/// names — the embedder always, the local-cortex extras (reranker + NLI) when
/// `cortex`; only the search-serving commands ask for them, brief/export/
/// import skip the load. Default selections keep their pre-selection loading
/// behavior (env-dir override, hf_hub fallback); user selections provision
/// from their recorded URLs into `~/.cache/engram/<name>/` first.
fn load_models(fake_embeddings: bool, cortex: bool) -> anyhow::Result<Models> {
    let cfg = engram_core::cortex::load();
    let embedder: Arc<dyn Embedder> = if fake_embeddings {
        tracing::warn!("using fake embeddings (search quality is degraded)");
        Arc::new(FakeEmbedder::default())
    } else {
        tracing::info!("loading local embedding model…");
        load_embedder(&cfg.effective(engram_core::cortex::Role::Embedding))
            .context("initializing the embedding model")?
    };
    let mut set = ModelSet {
        embedder,
        reranker: None,
        nli: None,
    };
    if cortex && !fake_embeddings {
        // The cortex is an upgrade, never a dependency: any failed load
        // (first run offline, cache wiped) degrades that layer away.
        match load_reranker(&cfg.effective(engram_core::cortex::Role::Reranker)) {
            Ok(r) => set.reranker = Some(r),
            Err(e) => tracing::warn!("reranker unavailable, search keeps hybrid order: {e}"),
        }
        match load_nli(&cfg.effective(engram_core::cortex::Role::Nli)) {
            Ok(n) => set.nli = Some(n),
            Err(e) => tracing::warn!("NLI unavailable, cortex hints disabled: {e}"),
        }
    }
    Ok(Models {
        set: Arc::new(std::sync::RwLock::new(set)),
    })
}

fn is_default_spec(role: engram_core::cortex::Role, spec: &engram_core::cortex::ModelSpec) -> bool {
    engram_core::cortex::presets(role)[0].name == spec.name
}

/// Provision (when needed) and load one embedding spec. The default model
/// keeps `FastEmbedder::new`'s dir-then-hf_hub behavior; anything else loads
/// strictly from its provisioned directory.
fn load_embedder(spec: &engram_core::cortex::ModelSpec) -> anyhow::Result<Arc<dyn Embedder>> {
    use engram_core::cortex::Role;
    if is_default_spec(Role::Embedding, spec) {
        // Provision the default like any selected model — the curl download
        // into the same dir FastEmbedder::new prefers. fastembed's flaky
        // hf_hub fallback inside new() becomes the last resort, not the plan.
        if let Err(e) = provision(Role::Embedding, spec) {
            tracing::warn!("couldn't pre-provision the embedding model ({e:#}); falling back");
        }
        return Ok(Arc::new(FastEmbedder::new()?));
    }
    let dir = provision(Role::Embedding, spec)?;
    let dim = spec
        .dim
        .with_context(|| format!("model {} has no dim recorded", spec.name))?;
    Ok(Arc::new(FastEmbedder::from_spec(
        &spec.name,
        &dir,
        dim,
        spec.pooling.as_deref() == Some("mean"),
    )?))
}

fn load_reranker(spec: &engram_core::cortex::ModelSpec) -> anyhow::Result<Arc<dyn Reranker>> {
    use engram_core::cortex::Role;
    if is_default_spec(Role::Reranker, spec) {
        // The long-open asymmetry (Problem 009yrpcyno9p): the NLI model had a
        // curl ensure, the reranker leaned on fastembed's flaky hf_hub
        // download and silently lost the precision layer when it failed.
        // Provision it like everything else; new() prefers the local dir.
        if let Err(e) = provision(Role::Reranker, spec) {
            tracing::warn!("couldn't pre-provision the reranker ({e:#}); falling back");
        }
        return Ok(Arc::new(engram_core::FastReranker::new()?));
    }
    let dir = provision(Role::Reranker, spec)?;
    Ok(Arc::new(engram_core::FastReranker::open_dir(&dir)?))
}

fn load_nli(spec: &engram_core::cortex::ModelSpec) -> anyhow::Result<Arc<dyn Nli>> {
    use engram_core::cortex::Role;
    if is_default_spec(Role::Nli, spec) {
        // The default NLI keeps its ENGRAM_NLI_DIR override + CLI download.
        ensure_nli_model();
        return Ok(Arc::new(engram_core::FastNli::new()?));
    }
    let dir = provision(Role::Nli, spec)?;
    Ok(Arc::new(engram_core::FastNli::from_dir(&dir)?))
}

/// Make sure a spec's files exist under `~/.cache/engram/<name>/`,
/// downloading what's missing (curl, atomic per file — the NLI pattern
/// generalized). Returns the model directory.
fn provision(
    role: engram_core::cortex::Role,
    spec: &engram_core::cortex::ModelSpec,
) -> anyhow::Result<std::path::PathBuf> {
    let dir = engram_core::cortex::cache_dir(&spec.name)
        .context("no home directory for the model cache")?;
    let files = engram_core::cortex::spec_files(role, spec);
    if files.iter().all(|(name, _)| dir.join(name).is_file()) {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    tracing::info!("downloading {} (one-time)…", spec.name);
    for (name, url) in files {
        let target = dir.join(&name);
        if target.is_file() {
            continue;
        }
        let part = dir.join(format!("{name}.part"));
        let ok = std::process::Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&part)
            .arg(&url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok || std::fs::rename(&part, &target).is_err() {
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("downloading {url} failed");
        }
    }
    Ok(dir)
}

/// Open one store as an engine wired to the shared models. This is the hub's
/// engine factory: every project store the daemon serves goes through here.
fn open_engine(db: &Path, models: &Models) -> engram_core::Result<Engine> {
    if let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| engram_core::Error::Io(format!("creating {}: {e}", dir.display())))?;
    }
    let set = models.current();
    // 0.7.*: tepin is the default store — an unmigrated graph.db silently
    // migrates at open, leaving the SQLite file behind as the backup. Gated
    // on a real embedder: migration regenerates vectors, and fake vectors
    // over a real graph would be noise (the fake-opening thin commands
    // bounce through the core anyway). Failure is non-fatal — the store
    // opens on SQLite exactly as before.
    let resolved = engram_core::resolve_db_path(db);
    if resolved.extension().is_some_and(|e| e == "db")
        && resolved.exists()
        && !set.embedder.is_fake()
    {
        match engram_core::migrate_to_tepin(
            &resolved,
            set.embedder.clone(),
            engram_core::AuditOrigin::daemon(),
        ) {
            Ok(s) => tracing::info!(
                "migrated {} nodes / {} edges to {} — {} is untouched (your backup)",
                s.nodes,
                s.edges,
                s.dst.display(),
                resolved.display()
            ),
            Err(e) => tracing::warn!("auto-migration to TepinDB failed, staying on SQLite: {e}"),
        }
    }
    let store = engram_core::open_store(db)?;
    let mut engine = Engine::with_store(store, Box::new(set.embedder.clone()));
    // Write-time code_ref checks resolve against the repo the DB lives in
    // (<root>/.engram/graph.db), falling back to the launch cwd.
    let root = db
        .canonicalize()
        .ok()
        .and_then(|p| {
            let dir = p.parent()?;
            (dir.file_name()? == ".engram").then(|| dir.parent().map(|r| r.to_path_buf()))?
        })
        .or_else(|| std::env::current_dir().ok());
    if let Some(root) = root {
        engine.set_repo_root(root);
    }
    // History layer (0.8.4): the sibling store beside the curated one, opened
    // by the engine when the graph's history config is enabled (opt-in).
    // The daemon opts into at-rest sealing (keyring / file-fallback key);
    // bodies written before a key existed are sealed by the harvester's
    // backlog pass.
    engine.enable_history_sealing();
    engine.set_history_path(engram_core::history::history_store_path(&resolved));
    if let Some(r) = &set.reranker {
        engine.set_reranker(Box::new(r.clone()));
    }
    if let Some(n) = &set.nli {
        engine.set_nli(Box::new(n.clone()));
    }
    // Guarded vector upgrades, model identity first (a swapped embedding
    // model rebuilds vector storage and re-embeds — PLAN §7A model
    // selection), then the composition catch-up. Never fatal at startup.
    match engine.ensure_embed_model() {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            "re-embedded {n} nodes for the {} embedding model",
            engine.embed_model_id().name
        ),
        Err(e) => tracing::warn!("embedding-model upgrade failed: {e}"),
    }
    match engine.ensure_embed_composition() {
        Ok(0) => {}
        Ok(n) => tracing::info!("re-embedded {n} nodes for the current embedding composition"),
        Err(e) => tracing::warn!("embedding-composition upgrade failed: {e}"),
    }
    Ok(engine)
}

/// Single-engine form, kept for export/import.
fn build_engine(db: &Path, fake_embeddings: bool, cortex: bool) -> anyhow::Result<Engine> {
    let models = load_models(fake_embeddings, cortex)?;
    open_engine(db, &models).with_context(|| format!("opening {}", db.display()))
}

/// A short-lived multi-project hub over one store (PLAN §7C) — the brief's
/// direct-open fallback when no daemon owns the store. Long-lived hubs are
/// the machine core's job ([`build_core_hub`]); nothing here registers or
/// keeps locks past the command.
fn build_hub(db: &Path, fake_embeddings: bool, cortex: bool) -> anyhow::Result<(Arc<Hub>, Models)> {
    let db = &engram_core::resolve_db_path(db);
    let models = load_models(fake_embeddings, cortex)?;
    let engine = open_engine(db, &models).with_context(|| format!("opening {}", db.display()))?;
    let factory_models = models.clone();
    let factory: engram_core::EngineFactory = Box::new(move |db| open_engine(db, &factory_models));
    let hub = Arc::new(Hub::new(Arc::new(Mutex::new(engine)), None, Some(factory)));
    Ok((hub, models))
}

/// The core hub launched outside any project (v0.6.2 serve-anywhere): the
/// home graph is the current project; registered repos open lazily.
fn build_core_hub(fake_embeddings: bool) -> anyhow::Result<(Arc<Hub>, Models)> {
    let home = registry::home_db_path().context("no home directory for the home graph")?;
    let models = load_models(fake_embeddings, true)?;
    let engine =
        open_engine(&home, &models).with_context(|| format!("opening {}", home.display()))?;
    let factory_models = models.clone();
    let factory: engram_core::EngineFactory = Box::new(move |db| open_engine(db, &factory_models));
    Ok((
        Arc::new(Hub::new_home(Arc::new(Mutex::new(engine)), Some(factory))),
        models,
    ))
}

/// The daemon's model-selection hands (PLAN §7A): resolve a `/models` request
/// to a spec, provision + load it, swap it into the live model set AND every
/// open engine, persist the choice, and — for embeddings — run the guarded
/// re-embed on each open store. Projects not open right now re-embed lazily
/// on their next open via the same `ensure_embed_model` guard.
struct CortexAdmin {
    models: Models,
    hub: Arc<Hub>,
    /// The core's lazy model slots: a hot-swap wraps the fresh model in a
    /// lazy handle and installs it as the slot the idle checker unloads (so
    /// unload keeps its chokepoint after a swap), and counts as model use —
    /// which is what makes `POST /models` while idle-unloaded just work.
    lazy: lazy::LazyModels,
    idle: Arc<engram_http::IdleTracker>,
}

impl engram_http::ModelAdmin for CortexAdmin {
    fn describe(&self) -> serde_json::Value {
        use engram_core::cortex::{self, Role};
        let cfg = cortex::load();
        let role_json = |role: Role| {
            let presets = cortex::presets(role);
            let custom = cfg
                .get(role)
                .filter(|s| !presets.iter().any(|p| p.name == s.name));
            serde_json::json!({
                "role": role.as_str(),
                "active": cfg.effective(role).name,
                "default": presets[0].name,
                "presets": presets,
                "custom": custom,
            })
        };
        serde_json::json!({
            "available": true,
            "fake_embeddings": self.models.current().embedder.is_fake(),
            "roles": [
                role_json(Role::Embedding),
                role_json(Role::Reranker),
                role_json(Role::Nli),
            ],
        })
    }

    fn apply(&self, request: serde_json::Value) -> engram_core::Result<serde_json::Value> {
        use engram_core::cortex::{self, Role};
        let role = Role::parse(request["role"].as_str().unwrap_or_default())?;
        let spec: cortex::ModelSpec = if let Some(name) = request["preset"].as_str() {
            cortex::presets(role)
                .into_iter()
                .find(|p| p.name == name)
                .ok_or_else(|| {
                    engram_core::Error::Project(format!(
                        "unknown preset {name:?} for role {}",
                        role.as_str()
                    ))
                })?
        } else if request["custom"].is_object() {
            serde_json::from_value(request["custom"].clone())?
        } else {
            return Err(engram_core::Error::Project(
                "pass either \"preset\" or \"custom\"".into(),
            ));
        };
        if role == Role::Embedding {
            if self.models.current().embedder.is_fake() {
                return Err(engram_core::Error::Project(
                    "this daemon runs --fake-embeddings — restart with real embeddings before switching models".into(),
                ));
            }
            if spec.dim.is_none() {
                return Err(engram_core::Error::Project(
                    "an embedding model needs \"dim\" (its vector width)".into(),
                ));
            }
        }
        let load_err = |e: anyhow::Error| engram_core::Error::Io(format!("{e:#}"));
        let mut reembedded = 0usize;
        // Every fresh model rides in a lazy wrapper so idle unload keeps its
        // chokepoint after a swap; loading it eagerly here is model use, so
        // the swap also ends any idle-unloaded state (see mark_loaded below).
        match role {
            Role::Embedding => {
                let embedder = self
                    .lazy
                    .swap_embedder(load_embedder(&spec).map_err(load_err)?);
                self.models.set.write().expect("model set lock").embedder = embedder.clone();
                for engine in self.hub.engines() {
                    let mut engine = engine.lock().expect("engine lock");
                    engine.set_embedder(Box::new(embedder.clone()));
                    reembedded += engine.ensure_embed_model()?;
                }
            }
            Role::Reranker => {
                let reranker = self
                    .lazy
                    .swap_reranker(load_reranker(&spec).map_err(load_err)?);
                self.models.set.write().expect("model set lock").reranker =
                    Some(reranker.clone() as Arc<dyn Reranker>);
                for engine in self.hub.engines() {
                    engine
                        .lock()
                        .expect("engine lock")
                        .set_reranker(Box::new(reranker.clone()));
                }
            }
            Role::Nli => {
                let nli = self.lazy.swap_nli(load_nli(&spec).map_err(load_err)?);
                self.models.set.write().expect("model set lock").nli =
                    Some(nli.clone() as Arc<dyn Nli>);
                for engine in self.hub.engines() {
                    engine
                        .lock()
                        .expect("engine lock")
                        .set_nli(Box::new(nli.clone()));
                }
            }
        }
        let now = engram_core::now();
        self.idle.mark_loaded(now);
        self.idle.touch_model_use();
        // Persist: a default selection is the absence of config.
        let mut cfg = cortex::load();
        cfg.set(role, (!is_default_spec(role, &spec)).then(|| spec.clone()));
        cortex::save(&cfg)?;
        Ok(serde_json::json!({
            "role": role.as_str(),
            "applied": spec.name,
            "reembedded_nodes": reembedded,
        }))
    }
}

/// Best-effort download of the default NLI model (curl, exactly like
/// self-update; direct from Hugging Face — the user's chosen distribution).
/// Atomic per file (.part + rename); any failure leaves the cortex hint-less,
/// never blocks startup.
fn ensure_nli_model() {
    let Some(dir) = engram_core::nli::nli_model_dir() else {
        return;
    };
    if engram_core::nli::NLI_MODEL_FILES
        .iter()
        .all(|f| dir.join(f).is_file())
    {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    const BASE: &str =
        "https://huggingface.co/techtheist/deberta-v3-small-tasksource-nli-onnx/resolve/main";
    let sources = [
        ("model.onnx", format!("{BASE}/onnx/model_quantized.onnx")),
        ("tokenizer.json", format!("{BASE}/tokenizer.json")),
        ("config.json", format!("{BASE}/config.json")),
    ];
    tracing::info!("downloading the NLI model (one-time, ~172 MB)…");
    for (name, url) in sources {
        let target = dir.join(name);
        if target.is_file() {
            continue;
        }
        let part = dir.join(format!("{name}.part"));
        let ok = std::process::Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&part)
            .arg(&url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok || std::fs::rename(&part, &target).is_err() {
            let _ = std::fs::remove_file(&part);
            tracing::warn!("NLI model download failed ({url}); cortex hints stay off");
            return;
        }
    }
}

/// The thin-client resolution (PLAN §7C): a healthy daemon that owns this db
/// — daemon.json next to the store, /health answers, and the advertised db
/// matches. When one exists, IT holds the file; everything else talks HTTP.
fn daemon_for(db: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(db.parent()?.join("daemon.json")).ok()?;
    let port = serde_json::from_str::<serde_json::Value>(&raw).ok()?["port"].as_u64()? as u16;
    let health = doctor::http_get(port, "/health")?;
    let canon = std::fs::canonicalize(db).unwrap_or_else(|_| db.to_path_buf());
    health
        .contains(&canon.display().to_string())
        .then_some(port)
}

/// Start the machine core detached (`engram-alpha core`, stdout/stderr →
/// `~/.engram/core.log`) and wait for its healthy advertisement. The model
/// load dominates a first run (provisioning downloads), so the health wait is
/// generous: 180s. Dev flags travel: `--fake-embeddings` is passed through,
/// env (ENGRAM_HOME, ENGRAM_* knobs) is inherited by the child.
fn spawn_core_and_wait(http_port: Option<u16>, fake_embeddings: bool) -> anyhow::Result<u16> {
    let exe = std::env::current_exe().context("locating this binary")?;
    let home = registry::engram_home().context("no home directory for the engram core")?;
    let _ = std::fs::create_dir_all(&home);
    let log_path = home.join("core.log");
    // A failed log create must not abort the spawn (the 0.8.5 lesson) — the
    // core just runs with its output discarded.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    let (stdout, stderr) = match log {
        Some(f) => match f.try_clone() {
            Ok(c) => (c.into(), f.into()),
            Err(_) => (std::process::Stdio::null(), f.into()),
        },
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("core");
    if let Some(port) = http_port {
        cmd.args(["--http-port", &port.to_string()]);
    }
    if fake_embeddings {
        cmd.arg("--fake-embeddings");
    }
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("spawning the engram core")?;
    for _ in 0..360 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(port) = machine_core() {
            return Ok(port);
        }
        // An immediately-dead child (unwritable home, broken install) must
        // not cost the full 180s poll. Exiting isn't always dying, though —
        // `core` exits 0 when another core won the startup race — so a core
        // that advertises healthy right after the exit still counts.
        if let Ok(Some(status)) = child.try_wait() {
            if let Some(port) = machine_core() {
                return Ok(port);
            }
            anyhow::bail!(
                "the engram core exited during startup ({status}) — see {}",
                log_path.display()
            )
        }
    }
    anyhow::bail!(
        "the engram core didn't come up within 180s — see {}",
        log_path.display()
    )
}

/// The single-flight ensure-core: exactly one caller pays the spawn; a
/// concurrent caller waits on the gate and finds the running core. Called
/// from the binding paths AFTER the stdio handshake answered (handshake-
/// first): a slow first-run provision or a failed boot must starve the bind
/// step — where `fail()` turns it into a real JSON-RPC error — never the
/// client's initialize (impatient clients kill a silent server at ~60s).
fn ensure_machine_core(fake_embeddings: bool) -> anyhow::Result<u16> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _flight = GATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(port) = machine_core() {
        return Ok(port);
    }
    tracing::info!("no engram core on this machine — starting one");
    spawn_core_and_wait(None, fake_embeddings)
}

/// The repo root a store path belongs to: the parent of its `.engram` dir.
/// Path-shaped only — the store file itself need not exist yet.
fn repo_root_of(db: &Path) -> Option<PathBuf> {
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

/// Where an MCP session for `db` should connect (thin-client resolution,
/// v0.6.2): a daemon launched from this repo, else the machine core with the
/// repo registered on it (so the scoped endpoint binds the session to THIS
/// project), else nothing — the caller decides between spawning a core and
/// opening the file directly.
fn resolve_mcp_target(db: &Path) -> Option<String> {
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
fn default_db_path() -> PathBuf {
    PathBuf::from(".engram/graph.db")
}

/// The bail every failed resolution shares: where to look and what to run.
fn no_core_error(what: &std::path::Path) -> anyhow::Error {
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

async fn run_mcp(args: McpArgs) -> anyhow::Result<()> {
    // This process is ALWAYS a bridge (0.8.8, generalizing the 0.8.5 tepin
    // rule to every backend): the core owns every store; mcp never opens one.
    match &args.db {
        // An explicit --db is an explicit answer: fixed project, roots
        // ignored, verbatim passthrough (the pre-roots doctrine).
        Some(raw) => run_mcp_fixed(raw, args.fake_embeddings).await,
        // No --db: bind by the client's MCP roots, falling back to cwd —
        // one global config entry serves every project (issue #4).
        None => run_mcp_roots(args.fake_embeddings).await,
    }
}

async fn run_mcp_fixed(raw_db: &Path, fake_embeddings: bool) -> anyhow::Result<()> {
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
    // Handshake-first: the resolver runs AFTER the stdio transport is
    // serving, so a core that boots slowly (first-run model provisioning
    // can take minutes) or not at all never leaves the client's initialize
    // unanswered — a failed resolution goes through the bridge's fail path
    // and answers held requests with the real error instead of silence.
    let resolve: engram_mcp::FixedResolver = Arc::new(move || {
        let target = |url: String| engram_mcp::ResolvedTarget {
            url,
            lease_root: lease_root.clone(),
        };
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
                // Better no tools than a stolen lock (0.8.5) — and better a
                // clear error in mcp.log than a silently heavy in-process
                // hub (0.8.8).
                return Err(no_core_error(&db));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
    engram_mcp::serve_stdio_bridge(engram_mcp::BridgeTarget::Fixed { resolve }).await
}

async fn run_mcp_roots(fake_embeddings: bool) -> anyhow::Result<()> {
    tracing::info!(
        "engram-alpha mcp v{} (pid {}, no --db — binding by client roots, then cwd, \
         then the default agent project, then home)",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
    );
    // IDE launchers spawn MCP servers from arbitrary cwds — `/`, a vanished
    // dir (the Windsurf JetBrains plugin does). A missing cwd must not kill
    // the bridge before roots can name the real project; "/" stands in and
    // the resolver's usability check decides what it can host.
    let fallback_root =
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(std::path::MAIN_SEPARATOR_STR));
    let resolve: engram_mcp::RootResolver = Arc::new(move |root: PathBuf| {
        // Handshake-first: the machine core (project-independent) is ensured
        // HERE, in the binding that runs after the stdio handshake answered
        // — never before serving, where a slow first-run provision or a
        // failed boot would starve the client's initialize into a ~60s kill.
        // Single-flight: a rebind racing the first bind waits on the gate
        // and finds the running core instead of spawning a second one.
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
        if std::fs::create_dir_all(&dot).is_err() && !dot.is_dir() {
            // The root can't host a project (unwritable IDE launch cwd like
            // `/`, a deleted dir): the machine-level default agent project
            // is the next rung, then the machine core's home graph — never
            // death. A later roots/list_changed naming a real project
            // rebinds away through the normal path.
            tracing::warn!(
                "root {} can't host a project (mkdir .engram failed) — trying the default \
                 agent project setting, then the home graph",
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
                    tracing::info!("binding the home graph (no default agent project configured)");
                    return Ok(engram_mcp::ResolvedTarget {
                        url: format!("http://127.0.0.1:{port}/mcp"),
                        lease_root: home,
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
                });
            }
            if std::time::Instant::now() >= deadline {
                return Err(no_core_error(&root));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
    engram_mcp::serve_stdio_bridge(engram_mcp::BridgeTarget::Roots {
        fallback_root,
        resolve,
    })
    .await
}

/// The pre-home rung of the db-less binding ladder: the machine-level
/// `default_agent_project` setting (`~/.engram/settings.json`, edited from
/// the pane's Settings surface, served by the core at GET /settings).
/// Fetched from the core AT BIND TIME — changing the setting affects future
/// bindings, never already-connected sessions. Returns the configured
/// project's MCP target, or None (unset, "home", or no longer registered) —
/// the caller falls to the home graph exactly as before the setting existed.
fn default_agent_target(port: u16) -> Option<engram_mcp::ResolvedTarget> {
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
    })
}

/// What a `serve` invocation should become — decided by looking at the
/// machine, not by flags (the one-smart-binary Principle, v0.6.2).
enum ServeRole {
    /// Serve with this store as the current project (repo dir, or explicit --db).
    Project(PathBuf),
    /// No project here (~, non-git dir, or a declined init): run the machine
    /// core with the home graph as the current project.
    CoreOnly,
}

fn serve_role(args: &ServeArgs) -> anyhow::Result<ServeRole> {
    let resolved = engram_core::resolve_db_path(&args.db);
    if args.db != Path::new(".engram/graph.db") {
        // An explicit --db is an explicit answer.
        return Ok(ServeRole::Project(resolved));
    }
    let cwd = std::env::current_dir()?;
    if resolved.exists() || cwd.join(".engram").exists() {
        return Ok(ServeRole::Project(resolved));
    }
    if cwd.join(".git").exists() {
        // A git repo without a graph: offer to init — never create silently.
        if propose_init(&cwd) {
            return Ok(ServeRole::Project(resolved));
        }
        eprintln!(
            "Skipped — you can initialize this repo later with `engram-alpha setup`, another `serve`, or from the pane."
        );
    }
    Ok(ServeRole::CoreOnly)
}

/// Interactive init consent: only on a real terminal, default yes.
fn propose_init(repo: &Path) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    eprint!(
        "No Engram graph in {} yet — initialize one here? [Y/n] ",
        repo.display()
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// A healthy machine core, if one runs: `~/.engram/daemon.json` (beside the
/// registry — new state folds into the existing file family) verified over
/// /health.
fn machine_core() -> Option<u16> {
    let path = registry::engram_home()?.join("daemon.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let port = serde_json::from_str::<serde_json::Value>(&raw).ok()?["port"].as_u64()? as u16;
    doctor::http_get(port, "/health").map(|_| port)
}

/// The machine core's advertised pid, from the same file [`machine_core`]
/// health-verifies.
fn machine_core_pid() -> Option<u32> {
    let raw = std::fs::read_to_string(registry::engram_home()?.join("daemon.json")).ok()?;
    Some(serde_json::from_str::<serde_json::Value>(&raw).ok()?["pid"].as_u64()? as u32)
}

/// The light launcher (0.8.8 process model): make sure the machine core is
/// running, hand it this repo's project, refresh the repo-local
/// advertisement, print the pane URL, and exit — the core keeps serving.
async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    if args.http_only {
        eprintln!(
            "note: --http-only is deprecated — serve now ensures the shared machine core and \
             exits; with this flag it stays up while the core is healthy (supervisor back-compat)."
        );
    }
    let port = match machine_core() {
        Some(port) => port,
        None => {
            eprintln!("starting the engram core…");
            spawn_core_and_wait(args.http_port, args.fake_embeddings)?
        }
    };
    match serve_role(&args)? {
        ServeRole::Project(db) => {
            ensure_gitignored(&db);
            // The store may not exist yet (the core creates it on first
            // open), so absolutize by hand — a canonicalize would fail and
            // leave a relative path in the registry.
            let db_abs = if db.is_absolute() {
                db.clone()
            } else {
                std::env::current_dir()?.join(&db)
            };
            // Registration is the whole handoff: the registry is the shared
            // awareness file, and the core opens registered stores lazily on
            // first access (pane switch, bridge connect).
            match repo_root_of(&db_abs).or_else(|| std::env::current_dir().ok()) {
                Some(root) => match registry::register(&root, &db_abs) {
                    Ok(entry) => {
                        tracing::info!("registered project '{}' with the core", entry.name);
                        // The repo-local advertisement plugins scrape the pane
                        // port from — pointing at the core.
                        write_daemon_file(&db_abs, port, machine_core_pid().unwrap_or_default());
                    }
                    Err(e) => tracing::warn!("couldn't register this repo: {e}"),
                },
                None => tracing::warn!("couldn't resolve this repo's root — not registered"),
            }
            // First-run nudge (PLAN §8): say once when installed assistants
            // aren't wired to this repo yet.
            if let Ok(cwd) = std::env::current_dir() {
                let unwired: Vec<&str> = setup::detect_agents()
                    .into_iter()
                    .filter(|a| !setup::is_wired(&cwd, a))
                    .collect();
                if !unwired.is_empty() {
                    eprintln!(
                        "detected {} without Engram wiring — run `engram-alpha setup` in this repo to connect them",
                        unwired.join(", ")
                    );
                }
            }
        }
        ServeRole::CoreOnly => {
            tracing::info!("no project here — the core serves the home graph");
        }
    }
    println!("Engram core running — pane: http://127.0.0.1:{port}");
    if args.http_only {
        // Back-compat blocking mode: a process supervisor watching this
        // invocation effectively watches the core's health through it —
        // exiting immediately would restart-loop it.
        block_while_core_healthy(port).await;
        eprintln!("the engram core on port {port} is gone — exiting");
    }
    Ok(())
}

/// Poll the core's /health and return when it stops answering (one retry to
/// ride out a transient hiccup).
async fn block_while_core_healthy(port: u16) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if doctor::http_get(port, "/health").is_none() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if doctor::http_get(port, "/health").is_none() {
                return;
            }
        }
    }
}

/// The machine core (0.8.8): the ONE process on this machine holding every
/// store lock, the loaded models, the pane, and the MCP endpoints. The HTTP
/// server is the foreground; SIGTERM, ctrl-c, and `POST /shutdown` all run
/// the same orchestrated exit. The pre-0.8.8 stdio-MCP-as-foreground shape
/// (daemon dying with its first client) is gone.
async fn run_core(args: CoreArgs) -> anyhow::Result<()> {
    // `--foreground` is documentation: the core always runs in its own
    // foreground — serve/mcp detach it at spawn time instead.
    let _ = args.foreground;
    // One core per machine: a healthy one makes this invocation a no-op.
    if let Some(port) = machine_core() {
        println!("Engram core already running — pane: http://127.0.0.1:{port}");
        return Ok(());
    }
    update::notify_on_newer_release();
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
    // Idle ONNX unload (the strict-spec Decision): every model slot becomes a
    // reload-capable lazy handle. Unload clears the slots; any demand —
    // HTTP request, background sweep, hot-swap — reloads single-flight.
    let lazy_models = lazy::LazyModels::install(&models, &hub, &runtime.idle);
    let admin = Arc::new(CortexAdmin {
        models: models.clone(),
        hub: hub.clone(),
        lazy: lazy_models.clone(),
        idle: runtime.idle.clone(),
    });
    // Per-project MCP endpoints (v0.6.2): a bridge from repo X connects to
    // /projects/{id}/mcp and its sessions treat X as the current project —
    // one service instance per project, minted lazily over the shared hub.
    let mcp_services: Arc<Mutex<std::collections::HashMap<String, engram_mcp::McpHttpService>>> =
        Arc::default();
    let mcp_hub = hub.clone();
    let scoped_mcp = axum::routing::any(
        move |axum::extract::Path(id): axum::extract::Path<String>, req: axum::extract::Request| {
            let services = mcp_services.clone();
            let hub = mcp_hub.clone();
            async move {
                use axum::response::IntoResponse;
                let canonical = match hub.resolve_id(&id) {
                    Ok(c) => c,
                    Err(e) => {
                        return (axum::http::StatusCode::NOT_FOUND, e.to_string()).into_response();
                    }
                };
                let mut service = services
                    .lock()
                    .unwrap()
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
    );
    let router = engram_http::router_hub_core(
        hub.clone(),
        Some(db_display.clone()),
        admin,
        std::sync::Arc::new(skillgen::SkillInstaller),
        runtime.clone(),
    )
    // The daemon-hosted MCP endpoints (PLAN §0 transport migration):
    // `engram-alpha mcp` bridges stdio sessions here, so exactly one
    // process — this core — holds every store (redb requires it).
    .route_service("/mcp", engram_mcp::streamable_http_service(hub.clone()))
    .route("/projects/{id}/mcp", scoped_mcp);
    // Outermost layer so it sees EVERY request, the /mcp routes included
    // (they are siblings of the API router, not inside it). The tracker
    // itself decides which paths count as HTTP activity — /health, /system,
    // /clients*, /shutdown and the /projects registry meta ops are exempt so
    // plugin health polls, lease pings, and `engram-alpha status` never keep
    // models warm.
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

    // Periodic librarian pass (PLAN §10 Phase 1): archive TTL-expired stale
    // provisional nodes and refresh the suspected-conflict queue. Purely local
    // math — judgment stays with Claude/the user (PLAN §7). First tick fires
    // at startup, then every 6 hours.
    let sweeper = hub.clone();
    let librarian_idle = runtime.idle.clone();
    let librarian = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The graph-write count observed after the last COMPLETED pass —
        // snapshotting after the pass absorbs the pass's own writes (decay
        // archives, queued suspects) so they never mark the graph dirty.
        let mut clean_epoch: Option<u64> = None;
        loop {
            tick.tick().await;
            // Idle-unload guard: scan_conflicts uses embedder + NLI, so an
            // unconditional tick would reload the models every 6 hours and
            // silently defeat the unload. When they are unloaded and nothing
            // was written since the last completed pass, there is nothing a
            // sweep could find that the last one didn't — skip. Any write or
            // harvest re-dirties; the sweep then runs and may reload models,
            // and the model use re-arms the idle clock afterwards.
            if librarian_idle.is_unloaded() && clean_epoch == Some(librarian_idle.graph_writes()) {
                tracing::info!(
                    "librarian: models idle-unloaded and nothing changed since the last pass — skipping the sweep"
                );
                continue;
            }
            // Every engine the core currently holds open — the machine core
            // sweeps all its projects, not just the launch one.
            let (mut archived_total, mut added_total) = (0usize, 0usize);
            for engine in sweeper.engines() {
                let result = {
                    let mut engine = engine.lock().unwrap();
                    // Sweep writes (decay archives, scan suspects) are the
                    // daemon's own — attribute them as such in the journal.
                    engine.set_audit_origin(engram_core::AuditOrigin::daemon());
                    engine
                        .decay(engine.graph_config().policy.decay_ttl_days, false)
                        .and_then(|archived| {
                            engine.scan_conflicts().map(|added| (archived.len(), added))
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
            if archived_total > 0 || added_total > 0 {
                tracing::info!(
                    "sweep: archived {archived_total} stale nodes, queued {added_total} suspected conflicts"
                );
            }
            clean_epoch = Some(librarian_idle.graph_writes());
        }
    });

    // History harvester (0.8.4): tail coding-assistant transcripts into each
    // project's history layer. Same detached-task shape as the librarian
    // sweep; the sweep interval is short because the born-in provenance
    // parking lot resolves "on next harvest tick". First tick fires at
    // startup — that's the backfill (months of sessions appear at once).
    let harvest_hub = hub.clone();
    let harvest_idle = runtime.idle.clone();
    let harvester_task = tokio::spawn(async move {
        let interval = std::env::var("ENGRAM_HARVEST_INTERVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 5)
            .unwrap_or(60);
        let mut harvester = engram_core::harvest::Harvester::first_wave();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Filesystem wake-ups (0.8.7): a transcript write pulls the next sweep
        // forward instead of waiting out the interval, so a note captured
        // seconds ago can already find the exchange it was born in. The TIMED
        // SWEEP REMAINS THE GUARANTEE — the watcher is an accelerator that is
        // allowed to fail, miss events, or not start at all.
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
    });

    // Idle ONNX unload checker (the strict-spec Decision): when ALL of — no
    // live bridge lease, no counted HTTP activity, no model use — have held
    // for ENGRAM_IDLE_UNLOAD_SECS (default 15 minutes), drop the three ONNX
    // sessions. The core itself stays resident; the next demand from any
    // path reloads lazily. 0 disables.
    let idle_secs = idle_unload_secs();
    let unload_task = (idle_secs > 0).then(|| {
        let runtime = runtime.clone();
        let lazy_models = lazy_models.clone();
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
    });

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
    // Closing the listener + loops; open sessions/SSE die with the process,
    // which is what tells bridges to exit right away.
    http.abort();
    librarian.abort();
    harvester_task.abort();
    if let Some(task) = &unload_task {
        task.abort();
    }
    // Drain in-flight engine work: taking each lock once means every store
    // operation that had started has committed before the process exits.
    for engine in hub.engines() {
        drop(engine.lock());
    }
    remove_repo_daemon_files(port);
    remove_machine_daemon_file();
    // Process exit releases every store lock the engines held; both backends
    // are crash-safe past the drain (fsync-per-commit / WAL).
    std::process::exit(0);
}

/// Resolve `/shutdown` (the watch), SIGTERM, and ctrl-c into one exit reason.
async fn wait_for_shutdown(rx: &mut tokio::sync::watch::Receiver<bool>) -> &'static str {
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
fn remove_repo_daemon_files(port: u16) {
    for entry in registry::load().projects {
        let Some(dir) = Path::new(&entry.db).parent() else {
            continue;
        };
        let file = dir.join("daemon.json");
        let advertised = std::fs::read_to_string(&file)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v["port"].as_u64());
        if advertised == Some(port as u64) {
            let _ = std::fs::remove_file(&file);
        }
    }
}

/// `ENGRAM_IDLE_UNLOAD_SECS`: how long ALL of (no live bridge lease, no
/// counted HTTP activity, no model use) must hold continuously before the
/// core drops its ONNX sessions. Default 900 (15 minutes); 0 disables the
/// unload entirely. Kept out of clap like the port so an auto-spawned core
/// honors it.
fn idle_unload_secs() -> u64 {
    std::env::var("ENGRAM_IDLE_UNLOAD_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900)
}

/// This process's resident set in MB (`ps`, works on macOS and Linux) —
/// sampled only around an unload to put honest numbers in the log line.
fn process_rss_mb() -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb / 1024)
}

/// The core's default port: `$ENGRAM_HTTP_PORT` (sandboxes and tests), else
/// 8787 — kept out of clap so an auto-spawned core (no flags) honors it too.
fn default_http_port() -> u16 {
    std::env::var("ENGRAM_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8787)
}

/// How long a filesystem wake-up waits before sweeping, so one transcript
/// write — which arrives as several syscalls — costs one sweep, not several.
const WATCH_DEBOUNCE_MS: u64 = 400;

/// Watch the directories the adapters are reading transcripts from, and poke
/// `wake` when anything in them changes (0.8.7).
///
/// Every failure here is soft, and deliberately so: no watcher, an
/// unwatchable directory, a platform without a backend, an event storm that
/// drops events — all of it degrades to exactly the behavior 0.8.4 shipped,
/// because the timed sweep still runs on its own interval. A freshness
/// accelerator that could break ingestion would be a bad trade; this one
/// cannot.
///
/// Directories are watched NON-recursively: the roots are already the exact
/// parents of discovered transcripts, so a new file lands in a watched
/// directory, while a whole new directory tree is picked up by the next timed
/// sweep and folded in when the watcher re-arms.
fn spawn_transcript_watcher(
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

enum Bound {
    Listener(tokio::net::TcpListener, u16),
    /// The port is held by another engram daemon — two serves raced and the
    /// other one won; this invocation converges instead of walking ports.
    CoreWonTheRace(u16),
}

/// Bind the requested port, or walk forward to the next free one. A taken
/// port is probed first: if THIS home's core answers /health there (the
/// advertised db is our home graph), that is the core this invocation should
/// join, never a reason to open a second port (v0.6.2: one core, one pane).
/// A foreign engram daemon — another user, or a sandboxed test home — is
/// just a taken port to walk past.
async fn bind_or_converge(start: u16) -> anyhow::Result<Bound> {
    const TRIES: u16 = 16;
    for offset in 0..TRIES {
        let port = start + offset;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => return Ok(Bound::Listener(l, port)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                let ours = doctor::http_get(port, "/health").is_some_and(|b| {
                    serde_json::from_str::<serde_json::Value>(&b)
                        .ok()
                        .and_then(|v| v["db"].as_str().map(str::to_string))
                        .zip(registry::home_db_path())
                        .is_some_and(|(db, home)| Path::new(&db) == home)
                });
                if ours {
                    return Ok(Bound::CoreWonTheRace(port));
                }
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("binding {addr}")),
        }
    }
    anyhow::bail!("no free port in {start}..{}", start + TRIES)
}

/// `~/.engram/daemon.json` — the machine core's advertisement, next to the
/// registry. Stale files are harmless: every reader health-checks the port.
fn write_machine_daemon_file(port: u16, db_display: &str) {
    let Some(home) = registry::engram_home() else {
        return;
    };
    let _ = std::fs::create_dir_all(&home);
    let body = serde_json::json!({
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "pid": std::process::id(),
        "db": db_display,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Err(e) = std::fs::write(home.join("daemon.json"), format!("{body:#}\n")) {
        tracing::warn!("couldn't write the machine daemon file: {e}");
    }
}

fn remove_machine_daemon_file() {
    if let Some(home) = registry::engram_home() {
        let _ = std::fs::remove_file(home.join("daemon.json"));
    }
}

/// `.engram/daemon.json`, next to the DB: how plugins and the skill find the
/// pane port (they regex-scrape `port` — keep the shape). Since 0.8.8 it
/// advertises the machine core (`pid` is the core's), written by `serve` and
/// removed by the core's orchestrated shutdown. Stale files are harmless —
/// readers health-check the port before trusting it.
fn write_daemon_file(db: &Path, port: u16, pid: u32) {
    let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let db_display = std::fs::canonicalize(db)
        .unwrap_or_else(|_| db.to_path_buf())
        .display()
        .to_string();
    let body = serde_json::json!({
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "pid": pid,
        "db": db_display,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Err(e) = std::fs::write(dir.join("daemon.json"), format!("{body:#}\n")) {
        tracing::warn!("couldn't write daemon.json: {e}");
    }
}

/// Best-effort: make sure the repo's `.gitignore` excludes the graph DB dir
/// (it's personal/local). Idempotent; never fatal.
fn ensure_gitignored(db: &Path) {
    let Some(top) = db.components().next() else {
        return;
    };
    let entry = format!("{}/", top.as_os_str().to_string_lossy());
    let gitignore = Path::new(".gitignore");
    let current = std::fs::read_to_string(gitignore).unwrap_or_default();
    if current.lines().any(|l| {
        let l = l.trim();
        l == entry || l == entry.trim_end_matches('/')
    }) {
        return;
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&entry);
    next.push('\n');
    if std::fs::write(gitignore, next).is_ok() {
        tracing::info!("added {entry} to .gitignore");
    }
}
