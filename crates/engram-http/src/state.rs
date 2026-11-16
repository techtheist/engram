//! Shared state: the hub, admin hooks, the client census and the idle tracker.

use crate::*;

pub(crate) type EventMap = Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>;

/// What `/models` needs from the daemon: describe the selection and apply a
/// new one (download, load, swap into every engine, re-embed). Implemented by
/// the CLI so this crate stays download-free; absent in library use.
pub trait ModelAdmin: Send + Sync {
    fn describe(&self) -> serde_json::Value;
    /// `request`: `{"role": "embedding"|"reranker"|"nli", "preset": name}` or
    /// `{"role": …, "custom": ModelSpec}`.
    fn apply(&self, request: serde_json::Value) -> engram_core::Result<serde_json::Value>;
}

/// Skill installation hands: generate the
/// capture skill from a graph's ontology and write it into the project.
/// Implemented CLI-side, where the canonical skill texts are embedded.
pub trait SkillAdmin: Send + Sync {
    /// Install the skill for `cfg` under `repo_root`: `{installed: true, path,
    /// generated, variant}`, or `{installed: false, symlink: true, path, target,
    /// note}` when the skill dir is a symlink into a source tree (left untouched).
    fn install(
        &self,
        repo_root: &std::path::Path,
        cfg: &engram_core::GraphConfig,
        variant: &str,
    ) -> engram_core::Result<serde_json::Value>;

    /// Bring a project's installed skills and instruction blocks up to this
    /// binary: the core calls it, off the async runtime, the first
    /// time a bridge binds `repo_root` in this core's life.
    fn upkeep(&self, _repo_root: &std::path::Path) {}
}

/// Shared server state: the hub (each engine behind its own `Mutex`, since a
/// SQLite connection is `!Sync`) and one live-update channel per project.
pub struct AppState {
    pub(crate) hub: Arc<Hub>,
    pub(crate) events: EventMap,
    /// The database this daemon was launched on, reported by `/health` so a
    /// client that discovered a port can verify it belongs to *this* repo.
    pub(crate) db_path: Option<String>,
    /// Daemon start time, for `/system`'s uptime.
    pub(crate) started: std::time::Instant,
    /// Model selection hands, when a daemon provides them.
    pub(crate) model_admin: Option<Arc<dyn ModelAdmin>>,
    /// Skill installation hands, when a daemon provides them.
    pub(crate) skill_admin: Option<Arc<dyn SkillAdmin>>,
    /// The machine core's process identity + client census + shutdown
    /// trigger. Only the core daemon sets this; without it the census and
    /// `/shutdown` routes answer 404.
    pub(crate) runtime: Option<Arc<CoreRuntime>>,
    /// Per-session already-injected node ids for /refs/match — the ambient
    /// hook's dedupe memory (bounded; evicted wholesale when it grows).
    pub(crate) refs_seen: Mutex<HashMap<String, std::collections::HashSet<String>>>,
    /// Running encrypt/decrypt migrations, keyed `{project}:{target}`
    /// — the poll target behind the pane's progress loader.
    pub(crate) enc_jobs: Arc<Mutex<HashMap<String, EncJob>>>,
    /// Bridge and project roots upkeep has settled (`None`: done — it runs
    /// once per project per core lifetime, 0.11.1), or the last time a root
    /// was looked up and named no project (`Some(unix secs)`).
    pub(crate) upkept: Mutex<HashMap<String, Option<i64>>>,
}

/// One whole-store encrypt/decrypt migration's observable state.
#[derive(Clone, Default)]
pub(crate) struct EncJob {
    pub(crate) done: Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) total: Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) running: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) error: Arc<Mutex<Option<String>>>,
}

// ---- process census (process-model refactor) -----------------------------

/// One connected client's lease. Bridges register on connect, renew alongside
/// their heartbeat, and deregister on clean exit; a lease that stops renewing
/// expires (crashed client) and is pruned lazily on read.
#[derive(Clone, Serialize)]
pub struct ClientLease {
    pub lease_id: String,
    pub pid: u32,
    pub kind: String,
    /// The MCP client behind the bridge (`clientInfo.name`). A fixed-target bridge
    /// registers before its client's initialize, so the name may arrive on the
    /// first renewal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Registered project id owning `root`, when the registry knows it.
    pub project: Option<String>,
    pub root: String,
    pub connected_at: i64,
    pub last_seen: i64,
}

/// A lease this old without a renewal belongs to a dead client. Renewals ride
/// the bridges' 15s heartbeat, so 45s means three missed beats.
/// (`ENGRAM_LEASE_TTL_SECS` overrides for tests.)
pub(crate) fn lease_ttl_secs() -> i64 {
    std::env::var("ENGRAM_LEASE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(45)
}

/// The idle-unload clocks the core compares against `ENGRAM_IDLE_UNLOAD_SECS`
/// (HTTP activity, lease, model use) and the residency `/system` reports. One
/// tracker because HTTP code and the CLI's model slots both stamp it.
///
/// [`IdleTracker::stamp_http_request`] counts every request except
/// observability and process management: `/health`, `/system`, `/clients*`,
/// `/shutdown` and the `/projects` registry ops (scoped forms normalized).
pub struct IdleTracker {
    /// Unix seconds of the last HTTP request that counts as activity.
    pub(crate) last_http: std::sync::atomic::AtomicI64,
    /// Unix seconds of the last model use (stamped by the CLI's lazy slots on
    /// every embed/rank/judge — background sweeps extend this without being
    /// HTTP activity, so an in-flight sweep is never cut mid-work).
    pub(crate) last_model_use: std::sync::atomic::AtomicI64,
    /// Unix seconds a live bridge lease was last seen (register, ping renewal,
    /// or the idle checker observing a non-empty census).
    pub(crate) last_lease_seen: std::sync::atomic::AtomicI64,
    /// Unix seconds the models were idle-unloaded; 0 = loaded.
    pub(crate) unloaded_since: std::sync::atomic::AtomicI64,
    /// Unix seconds the current residency began (start, or last reload).
    pub(crate) loaded_since: std::sync::atomic::AtomicI64,
    /// Monotonic count of graph mutations (curated ChangeEvents + harvester
    /// writes) — the librarian's dirty flag: when models are unloaded and
    /// this hasn't moved since its last completed pass, the 6h tick skips
    /// instead of silently reloading models to sweep an unchanged graph.
    pub(crate) graph_writes: std::sync::atomic::AtomicU64,
}

impl Default for IdleTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl IdleTracker {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicI64, AtomicU64};
        let now = engram_core::now();
        Self {
            last_http: AtomicI64::new(now),
            last_model_use: AtomicI64::new(now),
            last_lease_seen: AtomicI64::new(now),
            unloaded_since: AtomicI64::new(0),
            loaded_since: AtomicI64::new(now),
            graph_writes: AtomicU64::new(0),
        }
    }

    /// Stamp `last_http_activity` for a request path, unless the path is one
    /// of the exempt observability/process endpoints (see the type docs).
    pub fn stamp_http_request(&self, path: &str) {
        if Self::counts_as_http_activity(path) {
            self.last_http.store(engram_core::now(), ORD);
        }
    }

    /// The exemption rule, split out so it is testable and documented in one
    /// place. `path` is the raw request path (pre scope-rewrite).
    pub fn counts_as_http_activity(path: &str) -> bool {
        let core = if let Some(rest) = path.strip_prefix("/projects/") {
            match rest.split_once('/') {
                // Scoped graph route: /projects/x/search → "search".
                Some((_, tail)) if !tail.is_empty() => tail,
                // /projects/{id} registry meta op.
                _ => return false,
            }
        } else if path == "/projects" {
            return false;
        } else {
            path.trim_start_matches('/')
        };
        let head = core.split('/').next().unwrap_or("");
        !matches!(head, "health" | "system" | "shutdown" | "clients")
    }

    pub fn touch_model_use(&self) {
        self.last_model_use.store(engram_core::now(), ORD);
    }

    pub fn touch_lease(&self) {
        self.last_lease_seen.store(engram_core::now(), ORD);
    }

    pub fn note_graph_write(&self) {
        self.graph_writes.fetch_add(1, ORD);
    }

    pub fn graph_writes(&self) -> u64 {
        self.graph_writes.load(ORD)
    }

    pub fn last_http_activity(&self) -> i64 {
        self.last_http.load(ORD)
    }

    pub fn last_model_use(&self) -> i64 {
        self.last_model_use.load(ORD)
    }

    pub fn last_lease_seen(&self) -> i64 {
        self.last_lease_seen.load(ORD)
    }

    pub fn is_unloaded(&self) -> bool {
        self.unloaded_since.load(ORD) > 0
    }

    pub fn mark_unloaded(&self, now: i64) {
        self.unloaded_since.store(now, ORD);
    }

    /// Back to resident (startup completion, lazy reload, hot-swap). The
    /// state is an aggregate over the three sessions: ANY demand ends the
    /// idle-unloaded state, and the remaining slots reload on their own
    /// first use.
    pub fn mark_loaded(&self, now: i64) {
        self.unloaded_since.store(0, ORD);
        self.loaded_since.store(now, ORD);
    }

    /// The `/system.models_state` contract: `{"state":"loaded"|"unloaded_idle",
    /// "since":<unix>}` — `unloaded_idle` is the exact string the pane's idle
    /// badge matches on.
    pub fn models_state(&self) -> serde_json::Value {
        let unloaded = self.unloaded_since.load(ORD);
        if unloaded > 0 {
            json!({ "state": "unloaded_idle", "since": unloaded })
        } else {
            json!({ "state": "loaded", "since": self.loaded_since.load(ORD) })
        }
    }
}

/// All tracker stamps are independent wall-clock scalars — relaxed is enough.
pub(crate) const ORD: std::sync::atomic::Ordering = std::sync::atomic::Ordering::Relaxed;

/// The machine core's identity and client census — what `/system` reports
/// under `processes`, what `engram-alpha status` renders, and the shutdown
/// trigger `POST /shutdown` fires. Constructed by the core daemon only.
pub struct CoreRuntime {
    pub pid: u32,
    pub version: String,
    /// Unix seconds the core came up.
    pub started_at: i64,
    /// The engram home dir this core serves (`~/.engram`).
    pub home: String,
    /// Idle-unload clocks + model residency state (see [`IdleTracker`]).
    pub idle: Arc<IdleTracker>,
    pub(crate) clients: Mutex<HashMap<String, ClientLease>>,
    pub(crate) shutdown: tokio::sync::watch::Sender<bool>,
}

impl CoreRuntime {
    /// Build the runtime plus the receiver the core's main loop selects on:
    /// `/shutdown` flips the watch, the core runs its orchestrated exit.
    pub fn new(home: String) -> (Arc<Self>, tokio::sync::watch::Receiver<bool>) {
        let (shutdown, rx) = tokio::sync::watch::channel(false);
        (
            Arc::new(Self {
                pid: std::process::id(),
                version: engram_core::advertised_version(),
                started_at: engram_core::now(),
                home,
                idle: Arc::new(IdleTracker::new()),
                clients: Mutex::new(HashMap::new()),
                shutdown,
            }),
            rx,
        )
    }

    pub(crate) fn register(
        &self,
        pid: u32,
        kind: String,
        root: String,
        client: Option<String>,
    ) -> ClientLease {
        let now = engram_core::now();
        self.idle.touch_lease();
        let project = registry::load()
            .resolve_root(std::path::Path::new(&root))
            .map(|e| e.id.clone());
        let lease = ClientLease {
            lease_id: engram_core::id::new_id(),
            pid,
            kind,
            client,
            project,
            root,
            connected_at: now,
            last_seen: now,
        };
        self.clients
            .locked()
            .insert(lease.lease_id.clone(), lease.clone());
        lease
    }

    pub(crate) fn renew(
        &self,
        lease_id: &str,
        root: Option<String>,
        client: Option<String>,
    ) -> bool {
        match self.clients.locked().get_mut(lease_id) {
            Some(lease) => {
                lease.last_seen = engram_core::now();
                // A roots rebind moves the row instead of minting a second
                // lease — the census (and the pane) key on lease_id.
                if let Some(root) = root.filter(|r| *r != lease.root) {
                    lease.project = registry::load()
                        .resolve_root(std::path::Path::new(&root))
                        .map(|e| e.id.clone());
                    lease.root = root;
                }
                // The client name rides renewals too: a fixed-target bridge
                // registers before its stdio client's initialize named one.
                if client.is_some() {
                    lease.client = client;
                }
                // A renewing lease is a connected client: it keeps models
                // resident through the lease clock — deliberately NOT the
                // HTTP-activity clock (pings are exempt there).
                self.idle.touch_lease();
                true
            }
            None => false,
        }
    }

    pub(crate) fn remove(&self, lease_id: &str) -> bool {
        self.clients.locked().remove(lease_id).is_some()
    }

    /// Live leases, expired ones pruned on the way out (lazy expiry — no
    /// timer needed, every reader sees a fresh census).
    pub fn clients(&self) -> Vec<ClientLease> {
        let now = engram_core::now();
        let ttl = lease_ttl_secs();
        let mut clients = self.clients.locked();
        clients.retain(|_, l| now - l.last_seen <= ttl);
        let mut out: Vec<ClientLease> = clients.values().cloned().collect();
        out.sort_by_key(|l| l.connected_at);
        out
    }
}

impl AppState {
    pub fn new(engine: Engine) -> Self {
        Self::from_hub(Arc::new(Hub::single(engine)), None)
    }

    /// Build state around a shared engine and install the change listener that
    /// turns every mutation — from this API *or* from Claude over MCP — into an
    /// SSE message.
    pub fn shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self::from_hub(Arc::new(Hub::single_shared(engine)), None)
    }

    pub fn shared_with_db(engine: Arc<Mutex<Engine>>, db_path: Option<String>) -> Self {
        Self::from_hub(Arc::new(Hub::single_shared(engine)), db_path)
    }

    /// The full multi-project form: every engine the hub opens (now or later)
    /// gets a listener feeding that project's SSE channel.
    pub fn from_hub(hub: Arc<Hub>, db_path: Option<String>) -> Self {
        Self::from_hub_observed(hub, db_path, None)
    }

    /// [`AppState::from_hub`] with an optional idle tracker whose graph-write
    /// counter the change listener bumps — the librarian's dirty flag rides
    /// the same chokepoint every curated mutation (pane, MCP, imports, the
    /// librarian's own archives) already flows through.
    pub(crate) fn from_hub_observed(
        hub: Arc<Hub>,
        db_path: Option<String>,
        idle: Option<Arc<IdleTracker>>,
    ) -> Self {
        let events: EventMap = Arc::default();
        let ev = events.clone();
        hub.set_listener_factory(Box::new(move |project_id: &str| {
            let tx = channel(&ev, project_id);
            let idle = idle.clone();
            Box::new(move |change| {
                if let Some(idle) = &idle {
                    idle.note_graph_write();
                }
                let _ = tx.send(encode_event(&change));
            })
        }));
        Self {
            hub,
            events,
            db_path,
            started: std::time::Instant::now(),
            model_admin: None,
            skill_admin: None,
            runtime: None,
            refs_seen: Mutex::new(HashMap::new()),
            enc_jobs: Arc::default(),
            upkept: Mutex::default(),
        }
    }

    /// The launch project's engine, pane-stamped (see [`pane`]).
    pub(crate) fn engine(&self) -> MutexGuard<'_, Engine> {
        let mut guard = self.hub.current().engine.locked();
        guard.set_audit_origin(AuditOrigin::pane());
        guard
    }

    /// Resolve the request's project scope to an engine. `all` never lands
    /// here — the two fan-out reads (search, check_claim) special-case it
    /// before resolving; everywhere else the hub's refusal explains the rule.
    pub(crate) fn engine_arc(&self, scope: &Scope) -> Result<Arc<Mutex<Engine>>, AppError> {
        match &scope.0 {
            None => Ok(self.hub.current().engine.clone()),
            Some(sel) => Ok(self.hub.get(sel)?),
        }
    }

    /// The repo root code_refs resolve against. Scoped requests use the
    /// scoped engine's own root (set when its store was opened); the launch
    /// project falls back to the served DB path, then cwd.
    pub(crate) fn repo_root(&self) -> std::path::PathBuf {
        self.db_path
            .as_deref()
            .map(std::path::Path::new)
            .and_then(|db| {
                let dir = db.parent()?;
                if dir.file_name()? != ".engram" {
                    return None;
                }
                dir.parent()
            })
            .map_or_else(
                || std::env::current_dir().unwrap_or_else(|_| ".".into()),
                std::path::Path::to_path_buf,
            )
    }

    pub(crate) fn scoped_root(&self, engine: &Arc<Mutex<Engine>>) -> std::path::PathBuf {
        engine
            .locked()
            .repo_root()
            .map_or_else(|| self.repo_root(), std::path::Path::to_path_buf)
    }
}
