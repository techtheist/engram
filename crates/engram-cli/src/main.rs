//! `engram serve` — one local daemon per repo exposing HTTP + SSE (for the
//! pane) and the MCP stdio server (for Claude) over a single shared engine
//! bound to that repo's `.engram/graph.db` (PLAN §6B).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::{Parser, Subcommand};
use engram_core::{Embedder, Engine, ExportGraph, FakeEmbedder, FastEmbedder, Store};
use tracing_subscriber::EnvFilter;

mod setup;

#[derive(Parser)]
#[command(
    name = "engram",
    version,
    about = "Durable graph memory for AI coding assistants"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the full local daemon (HTTP + SSE + MCP over stdio).
    Serve(ServeArgs),
    /// Run only the MCP stdio server (no HTTP port). This is what an MCP client
    /// like Claude Code launches; safe to run alongside a separate pane daemon
    /// since both share the DB via SQLite WAL.
    Mcp(McpArgs),
    /// Export the whole graph as portable JSON (to a file or stdout).
    Export(ExportArgs),
    /// Import a JSON snapshot (upsert by id; idempotent).
    Import(ImportArgs),
    /// Print the session-start brief: a compact markdown digest of the graph's
    /// canon (conflicts, open work, principles, decisions, cautions).
    Brief(BriefArgs),
    /// Self-update: download the latest release for this platform, verify its
    /// checksum, and replace this binary in place.
    Update(UpdateArgs),
    /// Wire the current repository for AI assistants: MCP registration +
    /// capture instructions, from assets embedded in this binary. With no
    /// --cli, auto-detects which assistants are installed and wires those.
    Setup(SetupArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Path to the graph database (created if missing).
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Port for the HTTP API + SSE stream (bound to 127.0.0.1).
    #[arg(long, default_value_t = 8787)]
    http_port: u16,
    /// Use the deterministic fake embedder instead of downloading the local
    /// ONNX model — for offline use and quick smoke tests.
    #[arg(long)]
    fake_embeddings: bool,
    /// Serve only HTTP + SSE (no MCP). Use when running the pane standalone,
    /// without Claude attached over stdio.
    #[arg(long)]
    http_only: bool,
}

#[derive(clap::Args)]
struct McpArgs {
    /// Path to the graph database (created if missing).
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct ExportArgs {
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Write to this file instead of stdout.
    #[arg(long, short)]
    out: Option<PathBuf>,
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct ImportArgs {
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// JSON snapshot file to import.
    file: PathBuf,
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct BriefArgs {
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Character budget for the digest.
    #[arg(long, default_value_t = engram_core::policy::DEFAULT_BRIEF_CHARS)]
    max_chars: usize,
    #[arg(long)]
    fake_embeddings: bool,
}

#[derive(clap::Args)]
struct UpdateArgs {
    /// Release tag to install (e.g. v0.1.16). Default: the latest release.
    #[arg(long)]
    version: Option<String>,
}

#[derive(clap::Args)]
struct SetupArgs {
    /// Assistants to wire, comma-separated: claude|codex|gemini|opencode|kilo|antigravity|all.
    /// Default: auto-detect what's installed.
    #[arg(long)]
    cli: Option<String>,
    /// Capture intensity for the installed instructions/skill.
    #[arg(long, default_value = "relaxed", value_parser = ["relaxed", "normal", "aggressive"])]
    skill: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to STDERR: stdout is the MCP protocol channel and must stay clean.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Serve(args) => serve(args).await,
        Command::Mcp(args) => run_mcp(args).await,
        Command::Export(args) => run_export(args),
        Command::Import(args) => run_import(args),
        Command::Brief(args) => run_brief(args),
        Command::Update(args) => run_update(args),
        Command::Setup(args) => run_setup(args),
    }
}

fn run_setup(args: SetupArgs) -> anyhow::Result<()> {
    let agents: Vec<&str> = match args.cli.as_deref() {
        Some("all") => setup::AGENTS.to_vec(),
        Some(list) => {
            let mut v = Vec::new();
            for a in list.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                let known = setup::AGENTS.iter().find(|k| **k == a).with_context(|| {
                    format!(
                        "unknown --cli '{a}' (claude|codex|gemini|opencode|kilo|antigravity|all)"
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
    setup::Setup::new(&args.skill)?.run(&agents)
}

/// Self-update from GitHub Releases: download the platform asset, verify its
/// published sha256, and atomically swap the running binary. Uses the system
/// curl/tar (present on macOS, Linux, WSL, and Windows 10+) so the binary
/// doesn't carry an HTTP client for one command.
fn run_update(args: UpdateArgs) -> anyhow::Result<()> {
    const REPO: &str = "techtheist/engram";
    let (target, ext, bin_name) = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "tar.gz", "engram")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ("x86_64-unknown-linux-gnu", "tar.gz", "engram")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", "exe", "engram.exe")
    } else {
        anyhow::bail!("no prebuilt binaries for this platform — update from source instead");
    };

    let tag = match args.version {
        Some(v) => v,
        None => {
            let out = std::process::Command::new("curl")
                .args([
                    "-fsSL",
                    &format!("https://api.github.com/repos/{REPO}/releases/latest"),
                ])
                .output()
                .context("running curl (is it installed?)")?;
            anyhow::ensure!(out.status.success(), "could not query the latest release");
            let body = String::from_utf8_lossy(&out.stdout);
            body.split("\"tag_name\"")
                .nth(1)
                .and_then(|rest| rest.split('"').nth(1))
                .map(str::to_string)
                .context("parsing the latest release tag")?
        }
    };

    let asset = format!("engram-{tag}-{target}.{ext}");
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{asset}");
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
        "updated {} to {tag} — restart your daemon (engram serve)",
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

fn run_brief(args: BriefArgs) -> anyhow::Result<()> {
    let engine = build_engine(&args.db, args.fake_embeddings)?;
    println!("{}", engine.brief(args.max_chars)?);
    Ok(())
}

fn run_export(args: ExportArgs) -> anyhow::Result<()> {
    let engine = build_engine(&args.db, args.fake_embeddings)?;
    let snapshot = engine.export()?;
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
    let graph: ExportGraph = serde_json::from_str(&data).context("parsing export JSON")?;
    let engine = build_engine(&args.db, args.fake_embeddings)?;
    let summary = engine.import(graph)?;
    tracing::info!(
        "imported {} nodes / {} edges into {}",
        summary.nodes,
        summary.edges,
        args.db.display()
    );
    Ok(())
}

/// Open the store and pick an embedder — shared by `serve` and `mcp`.
fn build_engine(db: &Path, fake_embeddings: bool) -> anyhow::Result<Engine> {
    if let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    let embedder: Box<dyn Embedder> = if fake_embeddings {
        tracing::warn!("using fake embeddings (search quality is degraded)");
        Box::new(FakeEmbedder::default())
    } else {
        tracing::info!("loading local embedding model…");
        Box::new(FastEmbedder::new().context("initializing fastembed model")?)
    };
    Ok(Engine::new(store, embedder))
}

async fn run_mcp(args: McpArgs) -> anyhow::Result<()> {
    let engine = build_engine(&args.db, args.fake_embeddings)?;
    tracing::info!("MCP server ready on stdio (db: {})", args.db.display());
    engram_mcp::serve_stdio(engine).await
}

async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    ensure_gitignored(&args.db);
    let engine = Arc::new(Mutex::new(build_engine(&args.db, args.fake_embeddings)?));

    // HTTP + SSE on a background task; both interfaces share `engine`, so a
    // write from Claude (MCP) streams to the pane and vice versa.
    let db_display = std::fs::canonicalize(&args.db)
        .unwrap_or_else(|_| args.db.clone())
        .display()
        .to_string();
    let router = engram_http::router_shared_with_db(engine.clone(), db_display.clone());
    let (listener, port) = bind_with_fallback(args.http_port).await?;
    if port != args.http_port {
        tracing::warn!("port {} was taken; using {port} instead", args.http_port);
    }
    // Record where we actually landed so plugins/skills can discover the port.
    write_daemon_file(&args.db, port, &db_display);
    tracing::info!("HTTP + SSE listening on http://127.0.0.1:{port}");

    // First-run nudge: if installed assistants aren't wired to this repo's
    // graph yet, say so once at startup (PLAN §8 — the binary owns setup).
    if let Ok(cwd) = std::env::current_dir() {
        let unwired: Vec<&str> = setup::detect_agents()
            .into_iter()
            .filter(|a| !setup::is_wired(&cwd, a))
            .collect();
        if !unwired.is_empty() {
            tracing::info!(
                "detected {} without Engram wiring — run `engram setup` in this repo to connect them",
                unwired.join(", ")
            );
        }
    }

    // Periodic librarian pass (PLAN §10 Phase 1): archive TTL-expired stale
    // provisional nodes and refresh the suspected-conflict queue. Purely local
    // math — judgment stays with Claude/the user (PLAN §7). First tick fires
    // at startup, then every 6 hours.
    let sweeper = engine.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let result = {
                let engine = sweeper.lock().unwrap();
                engine
                    .decay(engram_core::policy::DECAY_TTL_DAYS, false)
                    .and_then(|archived| {
                        engine.scan_conflicts().map(|added| (archived.len(), added))
                    })
            };
            match result {
                Ok((archived, added)) if archived > 0 || added > 0 => {
                    tracing::info!(
                        "sweep: archived {archived} stale nodes, queued {added} suspected conflicts"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("sweep failed: {e}"),
            }
        }
    });

    if args.http_only {
        tracing::info!("http-only mode (no MCP)");
        axum::serve(listener, router).await?;
        return Ok(());
    }

    let http = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("http server stopped: {e}");
        }
    });

    // MCP over stdio is the foreground; when Claude disconnects, the daemon ends.
    tracing::info!("MCP server ready on stdio");
    let result = engram_mcp::serve_stdio_shared(engine).await;
    http.abort();
    remove_daemon_file(&args.db);
    result
}

/// Bind the requested port, or walk forward to the next free one (another
/// repo's daemon likely owns the default — one daemon per repo is the norm).
/// The real port lands in the daemon file for clients to discover.
async fn bind_with_fallback(start: u16) -> anyhow::Result<(tokio::net::TcpListener, u16)> {
    const TRIES: u16 = 16;
    for offset in 0..TRIES {
        let port = start + offset;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => return Ok((l, port)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e).with_context(|| format!("binding {addr}")),
        }
    }
    anyhow::bail!("no free port in {start}..{}", start + TRIES)
}

/// `.engram/daemon.json`, next to the DB: how plugins and the skill find the
/// actual port. Stale files are harmless — readers health-check the port and
/// match the advertised `db` before trusting it.
fn write_daemon_file(db: &Path, port: u16, db_display: &str) {
    let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let body = serde_json::json!({
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "pid": std::process::id(),
        "db": db_display,
    });
    if let Err(e) = std::fs::write(dir.join("daemon.json"), format!("{body:#}\n")) {
        tracing::warn!("couldn't write daemon.json: {e}");
    }
}

fn remove_daemon_file(db: &Path) {
    if let Some(dir) = db.parent() {
        let _ = std::fs::remove_file(dir.join("daemon.json"));
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
