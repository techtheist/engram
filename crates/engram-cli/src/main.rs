//! The engram-alpha CLI. One machine core per user (`engram-alpha core`,
//! detached, holding every store, the models, the pane and the MCP endpoints)
//! plus light access points: `serve` ensures it and registers the project,
//! `mcp` bridges a stdio client to it, `status`/`stop` observe and end it.

use engram_core::sync::{MutexExt, RwLockExt};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::{Parser, Subcommand};
use engram_core::{
    Embedder, Engine, ExportGraph, FakeEmbedder, FastEmbedder, Hub, Nli, Reranker, registry,
};
use tracing_subscriber::EnvFilter;

mod boot;
mod cli;
mod commands;
mod core_process;
mod daemon_files;
mod doctor;
mod gitignore;
mod lazy;
mod machine_core;
mod models;
mod setup;
mod shipped_skills;
mod skillgen;
mod skillsync;
mod update;
mod upkeep;
mod watchdog;

use boot::*;
use cli::*;
use commands::{graph_io::*, maintain::*, mcp::*, serve::*, status::*, stop::*};
use core_process::*;
use daemon_files::*;
use machine_core::*;
use models::*;

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
            // Exit without dropping the runtime: tokio's stdin reader blocks until the
            // client writes or closes stdin, and Runtime::drop would wait on it forever.
            // Nothing needs destructors here (lease withdrawn, logs unbuffered).
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

/// Logs go to stderr (stdout is the MCP channel). `mcp` also tees into
/// `.engram/mcp.log` (`ENGRAM_MCP_LOG=0` disables), since IDE clients swallow
/// stderr. RUST_LOG sets the level.
fn init_tracing(command: &Command) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{Layer, fmt};

    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = match command {
        Command::Mcp(args) if std::env::var("ENGRAM_MCP_LOG").as_deref() != Ok("0") => {
            // A db-less bridge logs into the cwd's .engram, except under --wired-only:
            // creating .engram is what marks a folder as wired, so an unwired cwd logs
            // into ~/.engram instead.
            let db = args.db.clone().unwrap_or_else(default_db_path);
            let unwired_cwd = args.db.is_none()
                && args.wired_only
                && !std::env::current_dir()
                    .map(|d| d.join(".engram").is_dir())
                    .unwrap_or(false);
            let log = if unwired_cwd {
                registry::engram_home().and_then(|home| mcp_log_file(&home.join("graph.db")))
            } else {
                mcp_log_file(&db)
            };
            log.map(|f| {
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
