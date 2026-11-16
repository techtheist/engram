//! Command-line arguments.

use crate::*;

#[derive(Parser)]
#[command(
    name = "engram-alpha",
    version,
    about = "Durable graph memory for AI coding assistants"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
pub(crate) struct ServeArgs {
    /// Path to the graph database to register with the core (created on
    /// first access).
    #[arg(long, default_value = ".engram/graph.db")]
    pub(crate) db: PathBuf,
    /// Port the core should listen on when this launch has to start it
    /// (127.0.0.1 only; walks forward when taken).
    #[arg(long)]
    pub(crate) http_port: Option<u16>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    ///
    /// For offline use and quick smoke tests — fake vectors are noise for
    /// real searches. Propagated to the core when this launch starts it.
    #[arg(long)]
    pub(crate) fake_embeddings: bool,
    /// Deprecated: stay in the foreground while the core is healthy.
    ///
    /// Pre-0.8.8 shape kept for process supervisors: serve now always
    /// ensures the shared machine core; with this flag it blocks while the
    /// core is healthy and exits when the core dies.
    #[arg(long)]
    pub(crate) http_only: bool,
}

#[derive(clap::Args)]
pub(crate) struct CoreArgs {
    /// Port for the HTTP API + pane (bound to 127.0.0.1; walks forward when
    /// taken). Default: $ENGRAM_HTTP_PORT, else 8787.
    #[arg(long)]
    pub(crate) http_port: Option<u16>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    pub(crate) fake_embeddings: bool,
    /// No-op: the core always runs in the foreground of its own process.
    ///
    /// `serve` and `mcp` detach it for you; this flag exists for humans
    /// starting a core by hand and changes nothing.
    #[arg(long)]
    pub(crate) foreground: bool,
}

#[derive(clap::Args)]
pub(crate) struct StatusArgs {
    /// Machine-readable output for scripts.
    #[arg(long)]
    pub(crate) json: bool,
    /// Only each registered project's setup: which version wired it, for
    /// which assistants, and what is due. Reads the registry, no core needed.
    #[arg(long)]
    pub(crate) setup: bool,
}

#[derive(clap::Args)]
pub(crate) struct McpArgs {
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
    pub(crate) db: Option<PathBuf>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    pub(crate) fake_embeddings: bool,
    /// Bind only folders that are already Engram projects.
    ///
    /// For a globally installed server that starts in every folder (the
    /// Claude Code plugin's): a roots/cwd folder binds only when it holds
    /// `.engram/` or sits inside a registered project — it is never turned
    /// into a new project. Any other folder binds the home graph with
    /// project writes refused until the folder is wired (`engram-alpha
    /// setup`). Ignored with --db.
    #[arg(long)]
    pub(crate) wired_only: bool,
}

#[derive(clap::Args)]
pub(crate) struct ExportArgs {
    /// Path to the graph database to export.
    #[arg(long, default_value = ".engram/graph.db")]
    pub(crate) db: PathBuf,
    /// Write to this file instead of stdout.
    #[arg(long, short)]
    pub(crate) out: Option<PathBuf>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    pub(crate) fake_embeddings: bool,
}

#[derive(clap::Args)]
pub(crate) struct ImportArgs {
    /// Path to the graph database to import into (created if missing).
    #[arg(long, default_value = ".engram/graph.db")]
    pub(crate) db: PathBuf,
    /// JSON snapshot file to import.
    pub(crate) file: PathBuf,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    ///
    /// Tests only — vectors written this way are noise for real searches.
    #[arg(long)]
    pub(crate) fake_embeddings: bool,
}

#[derive(clap::Args)]
pub(crate) struct BriefArgs {
    /// Path to the graph database to brief from.
    #[arg(long, default_value = ".engram/graph.db")]
    pub(crate) db: PathBuf,
    /// Character budget for the digest (default: the graph's configured
    /// brief budget).
    #[arg(long)]
    pub(crate) max_chars: Option<usize>,
    /// Use the deterministic fake embedder instead of the local ONNX model.
    #[arg(long)]
    pub(crate) fake_embeddings: bool,
}

#[derive(clap::Args)]
pub(crate) struct UpdateArgs {
    /// Release tag to install (e.g. v0.8.7). Default: the latest release.
    #[arg(long)]
    pub(crate) version: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct DoctorArgs {
    /// Path to the graph database this repo is expected to use.
    #[arg(long, default_value = ".engram/graph.db")]
    pub(crate) db: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct MigrateArgs {
    /// The SQLite graph to migrate (the .tepin file lands next to it).
    #[arg(long, default_value = ".engram/graph.db")]
    pub(crate) db: PathBuf,
    /// Rebuild an existing graph.tepin target instead of refusing.
    #[arg(long)]
    pub(crate) force: bool,
    /// Use the deterministic fake embedder (tests only — the migrated graph's
    /// vectors would be noise for real searches).
    #[arg(long)]
    pub(crate) fake_embeddings: bool,
}

#[derive(clap::Args)]
pub(crate) struct SetupArgs {
    /// Assistants to wire, comma-separated: claude|codex|gemini|opencode|kilo|antigravity|bob|windsurf|devin|cursor|all.
    ///
    /// Default: auto-detect what's installed. windsurf writes both global
    /// configs — ${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json and
    /// ~/.devin/mcp_config.json (db-less — the bridge binds by MCP roots) —
    /// plus the .windsurf rule and skills; devin wires this repository
    /// (.devin: local MCP config, skills, SessionStart hook); cursor wires
    /// this repository (.cursor: MCP config, skills, sessionStart hook); the
    /// rest wire this repository.
    #[arg(long)]
    pub(crate) cli: Option<String>,
    /// Capture intensity for the installed instructions/skill. Default: the
    /// variant this repo was set up with, else relaxed.
    #[arg(long, value_parser = ["relaxed", "normal", "aggressive"])]
    pub(crate) skill: Option<String>,
    /// Only register the MCP server (and git-ignore the graph) — skip the
    /// skill, instruction blocks, and hooks. For setups where something else
    /// already ships those, like the Claude Code plugin.
    #[arg(long)]
    pub(crate) mcp_only: bool,
    /// Only bring the installed skills and instruction blocks up to this
    /// binary — tracked ones included, which automatic upkeep leaves alone —
    /// without rewiring anything. Edited copies are still kept.
    #[arg(long, conflicts_with_all = ["cli", "mcp_only"])]
    pub(crate) refresh: bool,
}
