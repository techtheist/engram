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
    /// Archive stale provisional nodes: episodic past the TTL, volatile past half of it.
    Decay(DecayArgs),
    /// Print the session-start brief: a compact markdown digest of the graph's
    /// canon (conflicts, open work, principles, decisions, cautions).
    Brief(BriefArgs),
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
struct DecayArgs {
    #[arg(long, default_value = ".engram/graph.db")]
    db: PathBuf,
    /// Days a provisional episodic node may go unconfirmed before archiving.
    #[arg(long, default_value_t = 14)]
    ttl_days: i64,
    /// Print what would be archived without archiving it.
    #[arg(long)]
    dry_run: bool,
    /// Simulate the clock at this unix timestamp (implies --dry-run).
    #[arg(long)]
    as_of: Option<i64>,
    #[arg(long)]
    fake_embeddings: bool,
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
        Command::Decay(args) => run_decay(args),
        Command::Brief(args) => run_brief(args),
    }
}

fn run_brief(args: BriefArgs) -> anyhow::Result<()> {
    let engine = build_engine(&args.db, args.fake_embeddings)?;
    println!("{}", engine.brief(args.max_chars)?);
    Ok(())
}

fn run_decay(args: DecayArgs) -> anyhow::Result<()> {
    let engine = build_engine(&args.db, args.fake_embeddings)?;
    let ttl_secs = args.ttl_days * 24 * 60 * 60;
    if args.dry_run || args.as_of.is_some() {
        let ids = engine.decay_preview(ttl_secs, args.as_of)?;
        println!(
            "would archive {} stale node(s) (ttl {} days{}):",
            ids.len(),
            args.ttl_days,
            args.as_of
                .map(|t| format!(", as of {t}"))
                .unwrap_or_default()
        );
        for id in ids {
            println!("  {id}");
        }
        return Ok(());
    }
    let archived = engine.decay(ttl_secs)?;
    tracing::info!(
        "archived {} stale node(s) (ttl {} days) in {}",
        archived.len(),
        args.ttl_days,
        args.db.display()
    );
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

    // Trust decay runs itself: sweep at startup and daily. Archived nodes
    // notify the change listener, so the pane updates live over SSE.
    let sweeper = engine.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
        loop {
            tick.tick().await;
            let swept = sweeper
                .lock()
                .unwrap()
                .decay(engram_core::policy::DEFAULT_DECAY_TTL_SECS);
            match swept {
                Ok(ids) if !ids.is_empty() => {
                    tracing::info!("decay sweep archived {} stale node(s)", ids.len());
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("decay sweep failed: {e}"),
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
