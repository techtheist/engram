//! `engram-alpha serve` — one local daemon per repo exposing HTTP + SSE (for the
//! pane) and the MCP stdio server (for Claude) over a single shared engine
//! bound to that repo's `.engram/graph.db` (PLAN §6B).

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
mod setup;

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
    /// Diagnose this repo's Engram installation: store integrity, embedding
    /// model, daemon-vs-DB path match, and per-assistant wiring. Exits
    /// non-zero when something needs fixing.
    Doctor(DoctorArgs),
    /// Wire the current repository for AI assistants: MCP registration +
    /// capture instructions, from assets embedded in this binary. With no
    /// --cli, auto-detects which assistants are installed and wires those.
    Setup(SetupArgs),
    /// Move this repo's graph onto the TepinDB backend (PLAN §7C step 5):
    /// nodes + edges travel as the canonical JSON export (embeddings
    /// regenerated), the suspect queue and audit journal are carried over
    /// verbatim, and the old graph.db stays behind untouched as a backup.
    /// Every command picks up graph.tepin automatically afterwards.
    Migrate(MigrateArgs),
    /// Stop every engram process on this machine in one gesture — the core
    /// and any per-repo daemons (bridges exit on their own when the core
    /// goes). The primitive behind updates and repairs that need exclusive
    /// access to the stores.
    Stop,
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
    /// Assistants to wire, comma-separated: claude|codex|gemini|opencode|kilo|antigravity|all.
    /// Default: auto-detect what's installed.
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
        Command::Doctor(args) => doctor::run(&engram_core::resolve_db_path(&args.db)),
        Command::Setup(args) => run_setup(args),
        Command::Migrate(args) => run_migrate(args),
        Command::Stop => run_stop(),
    }
}

/// Cut them all at once: terminate every advertised engram daemon — the
/// machine core plus any per-repo daemons — discovered from the daemon files
/// we already keep (never by process-name matching), health-verified before
/// each kill so a stale file can't take down an unrelated reused pid.
fn run_stop() -> anyhow::Result<()> {
    let mut targets: Vec<(String, std::path::PathBuf)> = Vec::new();
    if let Some(home) = registry::engram_home() {
        targets.push(("machine core".into(), home.join("daemon.json")));
    }
    for entry in registry::load().projects {
        if let Some(dir) = Path::new(&entry.db).parent() {
            targets.push((entry.name.clone(), dir.join("daemon.json")));
        }
    }
    let mut stopped = 0;
    for (label, file) in targets {
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
            println!("stopped {label} (pid {pid}, port {port})");
            stopped += 1;
        } else {
            eprintln!("couldn't stop {label} (pid {pid}) — stop it manually");
        }
    }
    if stopped == 0 {
        println!("nothing to stop — no healthy engram daemons advertised");
    }
    Ok(())
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
    let src = Engine::new(
        engram_core::SqliteStore::open(&src_path)
            .with_context(|| format!("opening {}", src_path.display()))?,
        Box::new(models.current().embedder.clone()),
    );
    let graph = src.export()?;
    let suspects = src.store().all_suspects()?;
    let audit_total = src.store().audit_page(None, None, 1)?.total.max(0) as usize;
    let audit = src.store().audit_page(None, None, audit_total)?;

    let mut dst = Engine::new(
        engram_core::TepinStore::open(&dst_path)
            .with_context(|| format!("creating {}", dst_path.display()))?,
        Box::new(models.current().embedder.clone()),
    );
    dst.set_audit_origin(engram_core::AuditOrigin::cli());
    // Journal first (oldest-first so seq keeps chronological order), then the
    // graph — import appends its own "imported" row, which lands last, as the
    // migration's own mark in the history.
    for entry in audit.entries.iter().rev() {
        dst.store().add_audit(entry)?;
    }
    for s in &suspects {
        dst.store().upsert_suspect(s)?;
    }
    let summary = dst.import(graph)?;
    dst.store()
        .set_embed_version(src.store().embed_version()?)?;
    if !args.fake_embeddings {
        dst.store().set_embed_model(&engram_core::EmbedModelId {
            name: engram_core::rag::DEFAULT_EMBED_MODEL.to_string(),
            dim: engram_core::rag::EMBED_DIM,
        })?;
    }

    // Verify before declaring victory — counts must survive the move.
    let src_stats = src.store().stats()?;
    let dst_stats = dst.store().stats()?;
    anyhow::ensure!(
        src_stats.nodes == dst_stats.nodes && src_stats.edges == dst_stats.edges,
        "count mismatch after migration (nodes {} -> {}, edges {} -> {}) — graph.tepin is incomplete, graph.db is untouched",
        src_stats.nodes,
        dst_stats.nodes,
        src_stats.edges,
        dst_stats.edges
    );
    let dst_suspects = dst.store().all_suspects()?.len();
    anyhow::ensure!(
        suspects.len() == dst_suspects,
        "suspect queue mismatch after migration ({} -> {dst_suspects})",
        suspects.len()
    );

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
        suspects.len(),
        audit_total,
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
    const REPO: &str = "techtheist/engram";
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

    let asset = format!("engram-alpha-{tag}-{target}.{ext}");
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

fn run_brief(args: BriefArgs) -> anyhow::Result<()> {
    // Thin client first: a running daemon owns the store (and on TepinDB is
    // the only process allowed to). A repo-launched daemon serves this
    // project at /brief; the machine core serves it at the scoped route.
    let db = engram_core::resolve_db_path(&args.db);
    let brief_url = resolve_mcp_target(&db)
        .map(|mcp| mcp.replace("/mcp", &format!("/brief?max_chars={}", args.max_chars)));
    if let Some(url) = brief_url
        && let Some((port, path)) = url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.split_once('/'))
        && let Ok(port) = port.parse::<u16>()
        && let Some(text) = doctor::http_get(port, &format!("/{path}"))
    {
        println!("{}", text.trim_end());
        return Ok(());
    }
    // The hub form appends the home-graph section (PLAN §7C); a brief is a
    // read, so it deliberately does not touch the registry.
    let (hub, _) = build_hub(&db, args.fake_embeddings, false, false)?;
    println!("{}", hub.brief(args.max_chars)?);
    Ok(())
}

fn run_export(args: ExportArgs) -> anyhow::Result<()> {
    let engine = build_engine(&args.db, args.fake_embeddings, false)?;
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
    let mut engine = build_engine(&args.db, args.fake_embeddings, false)?;
    engine.set_audit_origin(engram_core::AuditOrigin::cli());
    let summary = engine.import(graph)?;
    tracing::info!(
        "imported {} nodes / {} edges into {}",
        summary.nodes,
        summary.edges,
        args.db.display()
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
    let store = engram_core::open_store(db)?;
    let set = models.current();
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

/// The multi-project hub (PLAN §7C): the launch project's engine plus a
/// factory that opens any registered store against the same model runtime.
/// `register` also upserts this repo into `~/.engram/registry.json` — that
/// file is how every other project becomes aware of this one.
fn build_hub(
    db: &Path,
    fake_embeddings: bool,
    cortex: bool,
    register: bool,
) -> anyhow::Result<(Arc<Hub>, Models)> {
    let db = &engram_core::resolve_db_path(db);
    let models = load_models(fake_embeddings, cortex)?;
    let engine = open_engine(db, &models).with_context(|| format!("opening {}", db.display()))?;
    let entry = match (register, engine.repo_root().map(Path::to_path_buf)) {
        (true, Some(root)) => match registry::register(&root, db) {
            Ok(entry) => Some(entry),
            Err(e) => {
                tracing::warn!("couldn't register this project in the registry: {e}");
                None
            }
        },
        _ => None,
    };
    let factory_models = models.clone();
    let factory: engram_core::EngineFactory = Box::new(move |db| open_engine(db, &factory_models));
    let hub = Arc::new(Hub::new(Arc::new(Mutex::new(engine)), entry, Some(factory)));
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
        match role {
            Role::Embedding => {
                let embedder = load_embedder(&spec).map_err(load_err)?;
                self.models.set.write().expect("model set lock").embedder = embedder.clone();
                for engine in self.hub.engines() {
                    let mut engine = engine.lock().expect("engine lock");
                    engine.set_embedder(Box::new(embedder.clone()));
                    reembedded += engine.ensure_embed_model()?;
                }
            }
            Role::Reranker => {
                let reranker = load_reranker(&spec).map_err(load_err)?;
                self.models.set.write().expect("model set lock").reranker = Some(reranker.clone());
                for engine in self.hub.engines() {
                    engine
                        .lock()
                        .expect("engine lock")
                        .set_reranker(Box::new(reranker.clone()));
                }
            }
            Role::Nli => {
                let nli = load_nli(&spec).map_err(load_err)?;
                self.models.set.write().expect("model set lock").nli = Some(nli.clone());
                for engine in self.hub.engines() {
                    engine
                        .lock()
                        .expect("engine lock")
                        .set_nli(Box::new(nli.clone()));
                }
            }
        }
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
    const BASE: &str = "https://huggingface.co/Xenova/nli-deberta-v3-small/resolve/main";
    let sources = [
        ("model.onnx", format!("{BASE}/onnx/model_quantized.onnx")),
        ("tokenizer.json", format!("{BASE}/tokenizer.json")),
        ("config.json", format!("{BASE}/config.json")),
    ];
    tracing::info!("downloading the NLI model (one-time, ~35 MB)…");
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

/// Start the daemon detached and wait for it to own the store. The model load
/// dominates startup, so the health wait is generous; `None` = it never came
/// up (offline first run, port trouble) and the caller falls back.
fn spawn_daemon_and_wait(db: &Path) -> Option<u16> {
    let exe = std::env::current_exe().ok()?;
    let log = std::fs::File::create(db.parent()?.join("serve.log")).ok()?;
    std::process::Command::new(exe)
        .args(["serve", "--http-only", "--db"])
        .arg(db)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().ok()?)
        .stderr(log)
        .spawn()
        .ok()?;
    for _ in 0..120 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        // The spawned serve may have become the core — or converged with one
        // that won a startup race; either way counts as up.
        if let Some(port) = daemon_for(db).or_else(machine_core) {
            return Some(port);
        }
    }
    None
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
    let root = db.canonicalize().ok().and_then(|p| {
        let dir = p.parent()?;
        (dir.file_name()? == ".engram").then(|| dir.parent().map(Path::to_path_buf))?
    })?;
    let body = serde_json::json!({ "path": root }).to_string();
    let id = doctor::http_post(port, "/projects", &body)
        .and_then(|resp| serde_json::from_str::<serde_json::Value>(&resp).ok())
        .and_then(|v| v["id"].as_str().map(str::to_string))?;
    Some(format!("http://127.0.0.1:{port}/projects/{id}/mcp"))
}

async fn run_mcp(args: McpArgs) -> anyhow::Result<()> {
    let db = engram_core::resolve_db_path(&args.db);
    // Thin client first (PLAN §7C): when a daemon owns stores on this
    // machine, bridge stdio to it — mandatory on a TepinDB store (redb
    // allows one process), and it puts MCP writes on the pane's SSE feed.
    let mut target = resolve_mcp_target(&db);
    if target.is_none() && engram_core::is_tepin_path(&db) {
        tracing::info!("tepin store with no core — starting one (it must own the file)");
        spawn_daemon_and_wait(&db);
        target = resolve_mcp_target(&db);
    }
    if let Some(url) = target {
        tracing::info!("bridging stdio MCP to {url} (db: {})", db.display());
        return engram_mcp::serve_stdio_proxy(&url).await;
    }
    // No daemon: open the store directly (SQLite coexists via WAL; a tepin
    // store works too as long as this stays the only process).
    // Registering keeps the registry fresh even for repos only ever opened
    // over MCP (PLAN §7C: serve/mcp/setup all register).
    let (hub, _) = build_hub(&db, args.fake_embeddings, true, true)?;
    tracing::info!("MCP server ready on stdio (db: {})", db.display());
    engram_mcp::serve_stdio_hub(hub).await
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

/// Absorb a `serve` into an already-running core: hand it the cwd project
/// (registering an existing graph, or offering init on a bare git repo) and
/// point the user at the one pane. Ten serves, one core, zero errors.
fn converge_with_core(port: u16) -> anyhow::Result<()> {
    if let Ok(cwd) = std::env::current_dir() {
        let should_register =
            cwd.join(".engram").exists() || (cwd.join(".git").exists() && propose_init(&cwd));
        if should_register {
            let body = serde_json::json!({ "path": cwd }).to_string();
            match doctor::http_post(port, "/projects", &body) {
                Some(_) => tracing::info!("registered {} with the running core", cwd.display()),
                None => tracing::warn!(
                    "couldn't register {} with the core on port {port}",
                    cwd.display()
                ),
            }
        }
    }
    println!("Engram core already running — pane: http://127.0.0.1:{port}");
    Ok(())
}

async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    // One core per machine: a healthy one absorbs this invocation.
    if let Some(port) = machine_core() {
        return converge_with_core(port);
    }
    let role = serve_role(&args)?;
    let (hub, models, db, db_display) = match role {
        ServeRole::Project(db) => {
            ensure_gitignored(&db);
            let (hub, models) = build_hub(&db, args.fake_embeddings, true, true)?;
            let display = std::fs::canonicalize(&db)
                .unwrap_or_else(|_| db.clone())
                .display()
                .to_string();
            (hub, models, Some(db), display)
        }
        ServeRole::CoreOnly => {
            let (hub, models) = build_core_hub(args.fake_embeddings)?;
            let display = registry::home_db_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            tracing::info!("no project here — running the machine core over the home graph");
            (hub, models, None, display)
        }
    };

    let admin = Arc::new(CortexAdmin {
        models: models.clone(),
        hub: hub.clone(),
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
    let router = engram_http::router_hub_with_models(hub.clone(), Some(db_display.clone()), admin)
        // The daemon-hosted MCP endpoints (PLAN §0 transport migration):
        // `engram-alpha mcp` bridges stdio sessions here, so exactly one
        // process — this core — holds every store (redb requires it).
        .route_service("/mcp", engram_mcp::streamable_http_service(hub.clone()))
        .route("/projects/{id}/mcp", scoped_mcp);
    let (listener, port) = match bind_or_converge(args.http_port).await? {
        Bound::Listener(l, port) => (l, port),
        Bound::CoreWonTheRace(port) => return converge_with_core(port),
    };
    if port != args.http_port {
        tracing::warn!("port {} was taken; using {port} instead", args.http_port);
    }
    // Record where we landed: the machine-level daemon file always (how every
    // thin client finds the core), the repo-local one too when a project is
    // current (how pre-0.6.2 clients find their daemon).
    write_machine_daemon_file(port, &db_display);
    if let Some(db) = &db {
        write_daemon_file(db, port, &db_display);
    }
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
                "detected {} without Engram wiring — run `engram-alpha setup` in this repo to connect them",
                unwired.join(", ")
            );
        }
    }

    // Periodic librarian pass (PLAN §10 Phase 1): archive TTL-expired stale
    // provisional nodes and refresh the suspected-conflict queue. Purely local
    // math — judgment stays with Claude/the user (PLAN §7). First tick fires
    // at startup, then every 6 hours.
    let sweeper = hub.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
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
                        .decay(engram_core::policy::DECAY_TTL_DAYS, false)
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
    let result = engram_mcp::serve_stdio_hub(hub).await;
    http.abort();
    if let Some(db) = &db {
        remove_daemon_file(db);
    }
    remove_machine_daemon_file();
    result
}

enum Bound {
    Listener(tokio::net::TcpListener, u16),
    /// The port is held by another engram daemon — two serves raced and the
    /// other one won; this invocation converges instead of walking ports.
    CoreWonTheRace(u16),
}

/// Bind the requested port, or walk forward to the next free one. A taken
/// port is probed first: if an engram daemon answers /health there, that is
/// the core this invocation should join, never a reason to open a second
/// port (v0.6.2: one core, one pane).
async fn bind_or_converge(start: u16) -> anyhow::Result<Bound> {
    const TRIES: u16 = 16;
    for offset in 0..TRIES {
        let port = start + offset;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => return Ok(Bound::Listener(l, port)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                let ours = doctor::http_get(port, "/health")
                    .is_some_and(|b| b.contains("\"status\"") && b.contains("\"db\""));
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
