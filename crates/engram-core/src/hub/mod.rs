//! The hub: one process serving N project stores plus the
//! user-level home graph. Per-repo storage is unchanged — the hub *opens*
//! files, it doesn't own data. Engines are opened lazily through a factory
//! the caller supplies (so model loading stays with the CLI and one model
//! runtime serves every store), and cross-project access is read-federation:
//! provenance-tagged hits with a locality prior, never replication. The one
//! shared write target is the home graph.

use crate::sync::MutexExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::engine::{ChangeEvent, Engine, Listener};
use crate::registry::{self, ALL_PROJECTS, HOME_PROJECT, ProjectEntry};
use crate::types::*;
use crate::{Error, Result};

mod federation;
mod sessions;

/// Opens an engine over one store file — supplied by the CLI so every engine
/// shares the same (already-loaded) model runtimes.
pub type EngineFactory = Box<dyn Fn(&Path) -> Result<Engine> + Send + Sync>;
/// Reachable sibling engines, by project name.
type NamedEngines = Vec<(String, Arc<Mutex<Engine>>)>;
/// Builds the change listener for a freshly-opened project engine (the HTTP
/// layer wires these to per-project SSE channels). Keyed by project id.
pub type ListenerFactory = Box<dyn Fn(&str) -> Listener + Send + Sync>;

/// A judged contradiction landing anywhere the hub can see (a
/// `conflicts-with` edge created or retyped-into) — the mid-session push's
/// payload. Ids only: enrichment (titles) happens at delivery time, where an
/// engine can be locked safely.
#[derive(Debug, Clone)]
pub struct ConflictAlert {
    pub project: String,
    pub edge_id: String,
    pub from_id: String,
    pub to_id: String,
}

/// A dependency-free broadcast: a ring of recent alerts with a global
/// sequence; every subscriber keeps its own cursor. Subscribers that never
/// drain cost nothing; the ring caps memory.
pub struct AlertBus {
    inner: Mutex<(u64, std::collections::VecDeque<(u64, ConflictAlert)>)>,
}

const ALERT_RING: usize = 128;

impl AlertBus {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new((0, std::collections::VecDeque::new())),
        })
    }

    fn push(&self, alert: ConflictAlert) {
        let mut inner = self.inner.locked();
        inner.0 += 1;
        let seq = inner.0;
        inner.1.push_back((seq, alert));
        if inner.1.len() > ALERT_RING {
            inner.1.pop_front();
        }
    }
}

/// One subscriber's view of the bus: starts at "now", `drain` returns what
/// landed since the last drain.
pub struct ConflictFeed {
    bus: Arc<AlertBus>,
    cursor: u64,
}

impl ConflictFeed {
    pub fn drain(&mut self) -> Vec<ConflictAlert> {
        let inner = self.bus.inner.locked();
        let fresh: Vec<ConflictAlert> = inner
            .1
            .iter()
            .filter(|(seq, _)| *seq > self.cursor)
            .map(|(_, a)| a.clone())
            .collect();
        self.cursor = inner.0;
        fresh
    }
}

/// The listener every hub-owned engine gets: watches for live contradiction
/// edges and drops them on the bus. The verb is captured from the engine's
/// ontology at attach time (config edits that move the contradiction role
/// land at the next open).
fn attach_conflict_tap(engine: &Arc<Mutex<Engine>>, bus: Arc<AlertBus>, project: String) {
    let mut engine = engine.locked();
    let verb = engine.config().contradiction_verb().to_string();
    engine.add_listener(conflict_tap(bus, project, verb));
}

fn conflict_tap(bus: Arc<AlertBus>, project: String, contradiction_verb: String) -> Listener {
    Box::new(move |event| {
        let (ChangeEvent::EdgeAdded(edge) | ChangeEvent::EdgeUpdated(edge)) = &event else {
            return;
        };
        let live = matches!(edge.status, None | Some(EdgeStatus::Active));
        if edge.edge_type.as_str() == contradiction_verb && live && edge.valid_until.is_none() {
            bus.push(ConflictAlert {
                project: project.clone(),
                edge_id: edge.id.clone(),
                from_id: edge.from_id.clone(),
                to_id: edge.to_id.clone(),
            });
        }
    })
}

/// One open project: identity plus its engine.
pub struct ProjectHandle {
    pub id: String,
    pub name: String,
    pub root: Option<PathBuf>,
    pub db: Option<PathBuf>,
    pub engine: Arc<Mutex<Engine>>,
    /// The backing file's on-disk identity at open time (issue #8): a cache
    /// hit re-stats the path, and a mismatch means the store was deleted or
    /// replaced under the handle — evict and reopen instead of serving the
    /// orphaned inode forever. `None` = unknown, never evicted on that.
    identity: Option<StoreIdentity>,
}

/// What makes an open store file "the same file": device + inode on unix,
/// creation time elsewhere (stable across writes, different for a recreated
/// file). Deleting `.engram/` doesn't disturb an open handle — the flock and
/// the reads live on the unlinked inode — so path equality alone can't tell
/// a live store from a corpse.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
struct StoreIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    created: Option<std::time::SystemTime>,
}

fn store_identity(path: &Path) -> Option<StoreIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(StoreIdentity {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        Some(StoreIdentity {
            created: meta.created().ok(),
        })
    }
}

/// Is the handle's backing file still the one it opened? Unknowns lean
/// "yes": a handle with no recorded db or identity is never evicted.
fn backing_intact(handle: &ProjectHandle) -> bool {
    let (Some(db), Some(opened)) = (&handle.db, handle.identity) else {
        return true;
    };
    store_identity(db) == Some(opened)
}

pub struct Hub {
    current: ProjectHandle,
    factory: Option<EngineFactory>,
    listener_factory: Mutex<Option<Arc<ListenerFactory>>>,
    /// Lazily-opened engines beyond the current one, keyed by project id
    /// (the home graph under the reserved [`HOME_PROJECT`] key).
    open: Mutex<HashMap<String, ProjectHandle>>,
    /// The mid-session conflict push's transport (v0.6.3).
    alerts: Arc<AlertBus>,
    /// History-layer reset epoch: bumped by the pane's wholesale
    /// delete so the running harvester drops its cursors and caches. A
    /// counter, not an event — the harvester compares at each sweep.
    history_epoch: std::sync::atomic::AtomicU64,
    /// Live MCP session bindings (in-session rebind: 0.8.9 set_project, brief(project) since 0.8.11): where each session
    /// actually is, which after an in-session rebind is NOT what the bridge's
    /// launch lease says — `/system` reads census truth from here.
    sessions: Mutex<HashMap<String, SessionBinding>>,
}

/// One live MCP session's binding, `/system`'s census row.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionBinding {
    pub session_id: String,
    /// Resolved project id (`home` for the home graph).
    pub project: String,
    pub name: String,
    pub since: i64,
    pub last_seen: i64,
    /// The end client's name as its bridge relayed it ("Windsurf",
    /// "claude-code"), once the session's initialize said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Which rung of the binding ladder bound the session ("roots", "cwd",
    /// "default-project", "home", "db", "brief"; absent = the route alone).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_by: Option<String>,
}

/// A session binding this stale (no tool call, and the transport never
/// dropped the service) belongs to a client that vanished without a close —
/// pruned lazily on read, like leases.
const SESSION_BINDING_TTL_SECS: i64 = 1800;

impl Hub {
    /// Full hub: the launch project's engine plus a factory for every other
    /// registered store. `current` carries the registry identity when the
    /// caller registered the repo (serve/mcp do); `None` falls back to a
    /// local-only identity.
    pub fn new(
        engine: Arc<Mutex<Engine>>,
        current: Option<ProjectEntry>,
        factory: Option<EngineFactory>,
    ) -> Self {
        let current = if let Some(e) = current {
            ProjectHandle {
                db: Some(e.resolved_db()),
                id: e.id,
                name: e.name,
                root: Some(PathBuf::from(e.root)),
                engine,
                identity: None,
            }
        } else {
            let root = engine.locked().repo_root().map(Path::to_path_buf);
            let name = root
                .as_deref()
                .and_then(Path::file_name)
                .map_or_else(|| "current".into(), |n| n.to_string_lossy().to_lowercase());
            ProjectHandle {
                id: "current".into(),
                name,
                root,
                db: None,
                engine,
                identity: None,
            }
        };
        let alerts = AlertBus::new();
        attach_conflict_tap(&current.engine, alerts.clone(), current.id.clone());
        Self {
            current,
            factory,
            listener_factory: Mutex::new(None),
            open: Mutex::new(HashMap::new()),
            alerts,
            history_epoch: std::sync::atomic::AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// The machine core launched outside any project (v0.6.2 serve-anywhere):
    /// the home graph IS the current project — `get("home")` resolves to this
    /// same engine via `is_current`, registered projects open lazily through
    /// the factory, and the pane's switcher does the navigating.
    pub fn new_home(engine: Arc<Mutex<Engine>>, factory: Option<EngineFactory>) -> Self {
        let alerts = AlertBus::new();
        attach_conflict_tap(&engine, alerts.clone(), HOME_PROJECT.into());
        Self {
            current: ProjectHandle {
                id: HOME_PROJECT.into(),
                name: HOME_PROJECT.into(),
                root: None,
                db: registry::home_db_path(),
                engine,
                identity: None,
            },
            factory,
            listener_factory: Mutex::new(None),
            open: Mutex::new(HashMap::new()),
            alerts,
            history_epoch: std::sync::atomic::AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Single-project hub (tests, library embedding): no factory, so every
    /// cross-project selector fails with a clear message.
    pub fn single(engine: Engine) -> Self {
        Self::new(Arc::new(Mutex::new(engine)), None, None)
    }

    pub fn single_shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self::new(engine, None, None)
    }

    pub fn current(&self) -> &ProjectHandle {
        &self.current
    }

    /// The current history-reset epoch — the harvester compares this at
    /// each sweep and forgets its cursors/caches when it moved.
    pub fn history_epoch(&self) -> u64 {
        self.history_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Signal every harvester that history state was wiped (pane wholesale
    /// delete): cursors and caches must be rebuilt from the stores.
    pub fn bump_history_epoch(&self) {
        self.history_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Subscribe to judged-conflict alerts across every project this hub
    /// owns; the feed starts at "now" and `drain` returns what landed since.
    pub fn subscribe_conflicts(&self) -> ConflictFeed {
        ConflictFeed {
            bus: self.alerts.clone(),
            cursor: self.alerts.inner.locked().0,
        }
    }

    /// Every engine this hub currently holds open — the launch project plus
    /// whatever the lazy factory opened (home included). A live model swap
    /// walks these; projects not open right now
    /// pick the new models up on their next open.
    pub fn engines(&self) -> Vec<Arc<Mutex<Engine>>> {
        let mut engines = vec![self.current.engine.clone()];
        engines.extend(self.open.locked().values().map(|h| h.engine.clone()));
        engines
    }

    pub fn current_engine(&self) -> Arc<Mutex<Engine>> {
        self.current.engine.clone()
    }

    /// Install the per-project listener factory and apply it to every engine
    /// already open (the HTTP layer calls this once, wiring SSE).
    pub fn set_listener_factory(&self, f: ListenerFactory) {
        let f = Arc::new(f);
        self.current
            .engine
            .locked()
            .add_listener(f(&self.current.id));
        for handle in self.open.locked().values() {
            handle.engine.locked().add_listener(f(&handle.id));
        }
        *self.listener_factory.locked() = Some(f);
    }

    fn is_current(&self, selector: &str) -> bool {
        selector == self.current.id || selector == self.current.name
    }

    /// Does this path selector point inside the launch project's root?
    fn path_selects_current(&self, selector: &str) -> bool {
        let (Some(path), Some(root)) = (registry::selector_path(selector), &self.current.root)
        else {
            return false;
        };
        let canon = path.canonicalize().unwrap_or(path);
        canon.starts_with(root.canonicalize().as_ref().unwrap_or(root))
    }

    /// Resolve a selector to an engine: `None`-equivalent handled by callers
    /// (they pass the current engine directly), `home` opens the home graph,
    /// a name/id opens that registered project. `all` never resolves — it is
    /// a read fan-out, not a single engine.
    pub fn get(&self, selector: &str) -> Result<Arc<Mutex<Engine>>> {
        if selector == ALL_PROJECTS {
            return Err(Error::Project(
                "'all' fans a read out across every project (search/check_claim); it is not a \
                 single graph — write to the current project, a named one, or 'home'"
                    .into(),
            ));
        }
        if self.is_current(selector) {
            return Ok(self.current.engine.clone());
        }
        if selector == HOME_PROJECT {
            return self.open_home();
        }
        let reg = registry::load();
        let Some(entry) = reg.resolve(selector).cloned() else {
            // An unregistered path still names the launch project when it
            // points inside it — the registry is consulted first so a nested
            // project keeps winning its own directory.
            if self.path_selects_current(selector) {
                return Ok(self.current.engine.clone());
            }
            let mut known: Vec<String> = reg.projects.iter().map(|p| p.name.clone()).collect();
            known.push(self.current.name.clone());
            known.push(HOME_PROJECT.into());
            known.sort();
            known.dedup();
            return Err(Error::Project(format!(
                "unknown project '{selector}' — known: {}",
                known.join(", ")
            )));
        };
        self.open_entry(&entry)
    }

    /// The stable project id behind a selector (SSE channels key on this).
    pub fn resolve_id(&self, selector: &str) -> Result<String> {
        if self.is_current(selector) {
            return Ok(self.current.id.clone());
        }
        if selector == HOME_PROJECT {
            return Ok(HOME_PROJECT.into());
        }
        registry::load()
            .resolve(selector)
            .map(|e| e.id.clone())
            .or_else(|| {
                self.path_selects_current(selector)
                    .then(|| self.current.id.clone())
            })
            .ok_or_else(|| Error::Project(format!("unknown project '{selector}'")))
    }

    fn factory(&self) -> Result<&EngineFactory> {
        self.factory.as_ref().ok_or_else(|| {
            Error::Project(
                "multi-project access is served by the daemon/MCP process; this instance was \
                 opened single-project"
                    .into(),
            )
        })
    }

    /// Whether this hub can reach other projects at all — single-project
    /// constructions (stdio `serve` on one db) have no factory, and surfaces
    /// like the brief fallback hint must stay silent there.
    pub fn is_multi(&self) -> bool {
        self.factory.is_some()
    }

    /// The root directory of a resolved project id, when it has one.
    pub fn project_root(&self, id: &str) -> Option<String> {
        if id == self.current.id {
            return self.current.root.as_ref().map(|p| p.display().to_string());
        }
        registry::load()
            .projects
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.root)
    }

    /// The display name for a resolved project id.
    pub fn project_name(&self, id: &str) -> String {
        if id == self.current.id {
            return self.current.name.clone();
        }
        registry::load()
            .projects
            .into_iter()
            .find(|p| p.id == id)
            .map_or_else(|| id.to_string(), |p| p.name)
    }

    fn open_entry(&self, entry: &ProjectEntry) -> Result<Arc<Mutex<Engine>>> {
        // The launch project is already open as `current`: name and id short-
        // circuit before this, but a path selector arrives through the
        // registry, and opening that entry would be a second handle on the
        // same store — fatal under TepinDB's single-owner lock.
        if self.entry_is_current(entry) {
            return Ok(self.current.engine.clone());
        }
        // Registry ids survive a `.engram/` wipe (register upserts by root),
        // so a bare cache hit here would keep serving a deleted store's
        // orphaned inode forever (issue #8) — verify the backing file first.
        {
            let mut open = self.open.locked();
            if let Some(h) = open.get(&entry.id) {
                if backing_intact(h) {
                    return Ok(h.engine.clone());
                }
                eprintln!(
                    "engram: store for '{}' was deleted or replaced on disk — reopening fresh",
                    entry.name
                );
                // Sessions still holding the old engine keep their Arc; the
                // orphaned inode's lock never conflicts with the fresh file.
                open.remove(&entry.id);
            }
        }
        let db = entry.resolved_db();
        let engine = (self.factory()?)(&db)?;
        Ok(self.insert_open(ProjectHandle {
            id: entry.id.clone(),
            name: entry.name.clone(),
            root: Some(PathBuf::from(&entry.root)),
            identity: store_identity(&db),
            db: Some(db),
            engine: Arc::new(Mutex::new(engine)),
        }))
    }

    fn open_home(&self) -> Result<Arc<Mutex<Engine>>> {
        // The machine core's launch graph IS the home graph (`new_home`): hand
        // out that engine. A second open would wait out the store's lock retry
        // (3 s on TepinDB) and fail — every project brief silently lost its
        // home section that way, 3 s late.
        if self.current.id == HOME_PROJECT {
            return Ok(self.current.engine.clone());
        }
        {
            let mut open = self.open.locked();
            if let Some(h) = open.get(HOME_PROJECT) {
                if backing_intact(h) {
                    return Ok(h.engine.clone());
                }
                eprintln!(
                    "engram: the home graph was deleted or replaced on disk — reopening fresh"
                );
                open.remove(HOME_PROJECT);
            }
        }
        let db = registry::home_db_path()
            .ok_or_else(|| Error::Io("no home directory for the home graph".into()))?;
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Io(format!("creating {}: {e}", dir.display())))?;
        }
        let engine = (self.factory()?)(&db)?;
        Ok(self.insert_open(ProjectHandle {
            id: HOME_PROJECT.into(),
            name: HOME_PROJECT.into(),
            root: None,
            identity: store_identity(&db),
            db: Some(db),
            engine: Arc::new(Mutex::new(engine)),
        }))
    }

    fn insert_open(&self, handle: ProjectHandle) -> Arc<Mutex<Engine>> {
        attach_conflict_tap(&handle.engine, self.alerts.clone(), handle.id.clone());
        if let Some(f) = self.listener_factory.locked().as_ref() {
            handle.engine.locked().add_listener(f(&handle.id));
        }
        // A racing open of the same project keeps the first handle.
        self.open
            .locked()
            .entry(handle.id.clone())
            .or_insert(handle)
            .engine
            .clone()
    }
}
