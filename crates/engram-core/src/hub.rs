//! The hub (PLAN §7C): one process serving N project stores plus the
//! user-level home graph. Per-repo storage is unchanged — the hub *opens*
//! files, it doesn't own data. Engines are opened lazily through a factory
//! the caller supplies (so model loading stays with the CLI and one model
//! runtime serves every store), and cross-project access is read-federation:
//! provenance-tagged hits with a locality prior, never replication. The one
//! shared write target is the home graph.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::engine::{ChangeEvent, Engine, Listener};
use crate::registry::{self, ALL_PROJECTS, HOME_PROJECT, ProjectEntry};
use crate::types::*;
use crate::{Error, Result};

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
        let mut inner = self.inner.lock().unwrap();
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
        let inner = self.bus.inner.lock().unwrap();
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

/// The listener every hub-owned engine gets: watches for live
/// `conflicts-with` edges and drops them on the bus.
fn conflict_tap(bus: Arc<AlertBus>, project: String) -> Listener {
    Box::new(move |event| {
        let edge = match &event {
            ChangeEvent::EdgeAdded(e) | ChangeEvent::EdgeUpdated(e) => e,
            _ => return,
        };
        let live = matches!(edge.status, None | Some(EdgeStatus::Active));
        if edge.edge_type == EdgeType::ConflictsWith && live && edge.valid_until.is_none() {
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
}

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
        let current = match current {
            Some(e) => ProjectHandle {
                id: e.id,
                name: e.name,
                root: Some(PathBuf::from(e.root)),
                db: Some(PathBuf::from(e.db)),
                engine,
            },
            None => {
                let root = engine.lock().unwrap().repo_root().map(Path::to_path_buf);
                let name = root
                    .as_deref()
                    .and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_else(|| "current".into());
                ProjectHandle {
                    id: "current".into(),
                    name,
                    root,
                    db: None,
                    engine,
                }
            }
        };
        let alerts = AlertBus::new();
        current
            .engine
            .lock()
            .unwrap()
            .add_listener(conflict_tap(alerts.clone(), current.id.clone()));
        Self {
            current,
            factory,
            listener_factory: Mutex::new(None),
            open: Mutex::new(HashMap::new()),
            alerts,
        }
    }

    /// The machine core launched outside any project (v0.6.2 serve-anywhere):
    /// the home graph IS the current project — `get("home")` resolves to this
    /// same engine via `is_current`, registered projects open lazily through
    /// the factory, and the pane's switcher does the navigating.
    pub fn new_home(engine: Arc<Mutex<Engine>>, factory: Option<EngineFactory>) -> Self {
        let alerts = AlertBus::new();
        engine
            .lock()
            .unwrap()
            .add_listener(conflict_tap(alerts.clone(), HOME_PROJECT.into()));
        Self {
            current: ProjectHandle {
                id: HOME_PROJECT.into(),
                name: HOME_PROJECT.into(),
                root: None,
                db: registry::home_db_path(),
                engine,
            },
            factory,
            listener_factory: Mutex::new(None),
            open: Mutex::new(HashMap::new()),
            alerts,
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

    /// Subscribe to judged-conflict alerts across every project this hub
    /// owns; the feed starts at "now" and `drain` returns what landed since.
    pub fn subscribe_conflicts(&self) -> ConflictFeed {
        ConflictFeed {
            bus: self.alerts.clone(),
            cursor: self.alerts.inner.lock().unwrap().0,
        }
    }

    /// Every engine this hub currently holds open — the launch project plus
    /// whatever the lazy factory opened (home included). A live model swap
    /// (PLAN §7A model selection) walks these; projects not open right now
    /// pick the new models up on their next open.
    pub fn engines(&self) -> Vec<Arc<Mutex<Engine>>> {
        let mut engines = vec![self.current.engine.clone()];
        engines.extend(
            self.open
                .lock()
                .expect("hub open map")
                .values()
                .map(|h| h.engine.clone()),
        );
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
            .lock()
            .unwrap()
            .add_listener(f(&self.current.id));
        for handle in self.open.lock().unwrap().values() {
            handle.engine.lock().unwrap().add_listener(f(&handle.id));
        }
        *self.listener_factory.lock().unwrap() = Some(f);
    }

    fn is_current(&self, selector: &str) -> bool {
        selector == self.current.id || selector == self.current.name
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

    fn open_entry(&self, entry: &ProjectEntry) -> Result<Arc<Mutex<Engine>>> {
        if let Some(h) = self.open.lock().unwrap().get(&entry.id) {
            return Ok(h.engine.clone());
        }
        let engine = (self.factory()?)(Path::new(&entry.db))?;
        Ok(self.insert_open(ProjectHandle {
            id: entry.id.clone(),
            name: entry.name.clone(),
            root: Some(PathBuf::from(&entry.root)),
            db: Some(PathBuf::from(&entry.db)),
            engine: Arc::new(Mutex::new(engine)),
        }))
    }

    fn open_home(&self) -> Result<Arc<Mutex<Engine>>> {
        if let Some(h) = self.open.lock().unwrap().get(HOME_PROJECT) {
            return Ok(h.engine.clone());
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
            db: Some(db),
            engine: Arc::new(Mutex::new(engine)),
        }))
    }

    fn insert_open(&self, handle: ProjectHandle) -> Arc<Mutex<Engine>> {
        handle
            .engine
            .lock()
            .unwrap()
            .add_listener(conflict_tap(self.alerts.clone(), handle.id.clone()));
        if let Some(f) = self.listener_factory.lock().unwrap().as_ref() {
            handle.engine.lock().unwrap().add_listener(f(&handle.id));
        }
        // A racing open of the same project keeps the first handle.
        self.open
            .lock()
            .unwrap()
            .entry(handle.id.clone())
            .or_insert(handle)
            .engine
            .clone()
    }

    /// Every project this hub can reach: current, home, then the registry.
    pub fn projects(&self) -> Vec<ProjectInfo> {
        let open = self.open.lock().unwrap();
        // A core launched outside any project has home AS its current
        // project (v0.6.2 serve-anywhere) — one row, both hats.
        let home_is_current = self.current.id == HOME_PROJECT;
        let mut out = vec![ProjectInfo {
            id: self.current.id.clone(),
            name: self.current.name.clone(),
            root: self.current.root.as_ref().map(|p| p.display().to_string()),
            db: self
                .current
                .db
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            current: true,
            home: home_is_current,
            open: true,
            last_seen: None,
        }];
        if let Some(db) = registry::home_db_path().filter(|_| !home_is_current) {
            out.push(ProjectInfo {
                id: HOME_PROJECT.into(),
                name: HOME_PROJECT.into(),
                root: None,
                db: db.display().to_string(),
                current: false,
                home: true,
                open: open.contains_key(HOME_PROJECT),
                last_seen: None,
            });
        }
        for p in registry::load().projects {
            if self.entry_is_current(&p) {
                continue;
            }
            out.push(ProjectInfo {
                open: open.contains_key(&p.id),
                id: p.id,
                name: p.name,
                root: Some(p.root),
                db: p.db,
                current: false,
                home: false,
                last_seen: Some(p.last_seen),
            });
        }
        out
    }

    fn entry_is_current(&self, p: &ProjectEntry) -> bool {
        p.id == self.current.id
            || self
                .current
                .root
                .as_ref()
                .is_some_and(|r| Path::new(&p.root) == r)
    }

    /// Sibling projects (registry minus current), opened lazily; unopenable
    /// ones are reported, never silently dropped (PLAN §7A: no silent caps).
    fn others(&self, include_home: bool) -> (NamedEngines, Vec<String>) {
        let mut engines = Vec::new();
        let mut skipped = Vec::new();
        if include_home && registry::home_db_path().is_some_and(|p| p.is_file()) {
            match self.open_home() {
                Ok(e) => engines.push((HOME_PROJECT.to_string(), e)),
                Err(err) => skipped.push(format!("{HOME_PROJECT}: {err}")),
            }
        }
        for entry in registry::load().projects {
            if self.entry_is_current(&entry) {
                continue;
            }
            if !Path::new(&entry.db).is_file() {
                skipped.push(format!("{}: db missing ({})", entry.name, entry.db));
                continue;
            }
            match self.open_entry(&entry) {
                Ok(e) => engines.push((entry.name, e)),
                Err(err) => skipped.push(format!("{}: {err}", entry.name)),
            }
        }
        (engines, skipped)
    }

    /// `project: "all"` — search the current project at full weight, then
    /// every sibling and the home graph under the locality prior, provenance
    /// on every foreign hit. Engines are locked one at a time.
    pub fn search_all(
        &self,
        query: &str,
        types: &[NodeType],
        limit: usize,
    ) -> Result<(Vec<SearchHit>, Vec<String>)> {
        let mut hits = self
            .current
            .engine
            .lock()
            .unwrap()
            .search(query, types, limit)?;
        let (others, mut skipped) = self.others(true);
        for (name, engine) in others {
            match engine.lock().unwrap().search(query, types, limit) {
                Ok(mut foreign) => {
                    for hit in &mut foreign {
                        hit.score *= crate::policy::CROSS_PROJECT_PRIOR;
                        hit.project = Some(name.clone());
                    }
                    hits.extend(foreign);
                }
                Err(e) => skipped.push(format!("{name}: {e}")),
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        Ok((hits, skipped))
    }

    /// `check_claim` across every reachable graph, buckets merged with
    /// provenance. The current project judges first; a sibling without the
    /// NLI layer is reported in `skipped`.
    pub fn check_claim_all(&self, text: &str, limit: usize) -> Result<(ClaimReport, Vec<String>)> {
        let mut report = self
            .current
            .engine
            .lock()
            .unwrap()
            .check_claim(text, limit)?;
        let (others, mut skipped) = self.others(true);
        for (name, engine) in others {
            match engine.lock().unwrap().check_claim(text, limit) {
                Ok(foreign) => {
                    let tag = |mut vs: Vec<ClaimVerdict>| {
                        for v in &mut vs {
                            v.project = Some(name.clone());
                        }
                        vs
                    };
                    report.supports.extend(tag(foreign.supports));
                    report.contradicts.extend(tag(foreign.contradicts));
                    report.silent.extend(tag(foreign.silent));
                }
                Err(e) => skipped.push(format!("{name}: {e}")),
            }
        }
        report
            .contradicts
            .sort_by(|a, b| b.contradiction.total_cmp(&a.contradiction));
        report
            .supports
            .sort_by(|a, b| b.entailment.total_cmp(&a.entailment));
        Ok((report, skipped))
    }

    /// The session brief: the current project's digest, a capped section of
    /// home-graph canon, and one line naming the other reachable graphs. The
    /// extras ride inside the same character budget so the total never
    /// exceeds what the caller asked for; with no siblings and no home graph
    /// they cost nothing.
    pub fn brief(&self, max_chars: usize) -> Result<String> {
        let home = self.home_brief_section(crate::policy::HOME_BRIEF_RESERVE);
        let roster = self.projects_brief_section();
        let reserve = home.as_deref().map(str::len).unwrap_or(0)
            + roster.as_deref().map(str::len).unwrap_or(0);
        let mut out = self
            .current
            .engine
            .lock()
            .unwrap()
            .brief(max_chars.saturating_sub(reserve))?;
        if let Some(section) = home {
            out.push_str(&section);
        }
        if let Some(section) = roster {
            out.push_str(&section);
        }
        Ok(out)
    }

    /// The brief's project roster (PLAN §7C awareness): which other graphs
    /// this session can reach and how. Emitted only when cross-project access
    /// actually works here (a factory is present) and something is reachable —
    /// advertising graphs a single-project instance can't open would mislead.
    fn projects_brief_section(&self) -> Option<String> {
        self.factory.as_ref()?;
        let mut names: Vec<String> = registry::load()
            .projects
            .iter()
            .filter(|p| !self.entry_is_current(p))
            .map(|p| p.name.clone())
            .collect();
        names.sort();
        if registry::home_db_path().is_some_and(|p| p.is_file()) {
            names.push(HOME_PROJECT.into());
        }
        if names.is_empty() {
            return None;
        }
        Some(format!(
            "\n## Other project graphs on this machine\nReachable via the `project` argument \
             most tools accept: {} — or `project: \"all\"` on search/check_claim to read across \
             everything (foreign hits carry provenance). Capture knowledge about a sibling \
             project into ITS graph; `home` holds user-level canon. `list_projects` has details.\n",
            names.join(", ")
        ))
    }

    /// The brief's home-graph section: top user-level principles and cautions,
    /// best-effort — never opens (and thus never creates) a home graph that
    /// doesn't exist yet, and any failure just drops the section.
    fn home_brief_section(&self, max_chars: usize) -> Option<String> {
        if !registry::home_db_path()?.is_file() {
            return None;
        }
        let engine = self.open_home().ok()?;
        let engine = engine.lock().unwrap();
        let mut out = String::new();
        for (node_type, cap) in [
            (NodeType::Principle, 4usize),
            (NodeType::Caution, 3),
            (NodeType::Decision, 2),
        ] {
            let nodes = engine.store().nodes_by_type_active(node_type, cap).ok()?;
            for n in nodes {
                let line = crate::engine::node_line(&n, crate::engine::EXCERPT_CHARS);
                if out.len() + line.len() + 1 > max_chars.saturating_sub(HOME_HEADING.len()) {
                    break;
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
        if out.is_empty() {
            return None;
        }
        Some(format!("{HOME_HEADING}{out}"))
    }

    /// Promotion nominations (PLAN §7C): current-project Principles/Cautions
    /// that recur (same type, high similarity) in other projects' graphs and
    /// aren't already represented in the home graph. Read-only — the user
    /// promotes from the pane; nothing self-applies.
    pub fn promotion_candidates(&self) -> Result<(Vec<PromotionCandidate>, Vec<String>)> {
        // Snapshot the current project's candidates with their vectors first,
        // then release its lock before touching any sibling engine.
        let mut seeds: Vec<(Node, Vec<f32>)> = Vec::new();
        {
            let engine = self.current.engine.lock().unwrap();
            for node_type in [NodeType::Principle, NodeType::Caution] {
                let total = engine.store().count_by_type_active(node_type)? as usize;
                for node in engine.store().nodes_by_type_active(node_type, total)? {
                    if let Some(vec) = engine.store().embedding_of(&node.id)? {
                        seeds.push((node, vec));
                    }
                }
            }
        }
        let (others, skipped) = self.others(false);
        let home = registry::home_db_path()
            .filter(|p| p.is_file())
            .and_then(|_| self.open_home().ok());

        let mut candidates = Vec::new();
        for (node, vec) in seeds {
            // Already promoted? A same-type near-duplicate in the home graph
            // means this knowledge is user-level already.
            if let Some(home) = &home
                && !similar_same_type(&home.lock().unwrap(), &vec, node.node_type)?.is_empty()
            {
                continue;
            }
            let mut matches = Vec::new();
            for (name, engine) in &others {
                for (id, title, similarity) in
                    similar_same_type(&engine.lock().unwrap(), &vec, node.node_type)?
                {
                    matches.push(PromotionMatch {
                        project: name.clone(),
                        id,
                        title,
                        similarity,
                    });
                }
            }
            if !matches.is_empty() {
                matches.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
                candidates.push(PromotionCandidate { node, matches });
            }
        }
        candidates.sort_by(|a, b| {
            let best =
                |c: &PromotionCandidate| c.matches.first().map(|m| m.similarity).unwrap_or(0.0);
            best(b).total_cmp(&best(a))
        });
        Ok((candidates, skipped))
    }
}

const HOME_HEADING: &str = "\n## Home graph (user-level canon, shared across projects)\n";

/// Active same-type nodes in `engine`'s store within the promotion band.
fn similar_same_type(
    engine: &Engine,
    vec: &[f32],
    node_type: NodeType,
) -> Result<Vec<(String, String, f64)>> {
    let mut out = Vec::new();
    for (id, distance) in engine.store().search_vec(vec, 5)? {
        let similarity = 1.0 - distance;
        if similarity < crate::policy::PROMOTION_SIMILARITY {
            break; // distance-ordered
        }
        if let Some(n) = engine.store().get_node(&id)?
            && n.node_type == node_type
            && n.valid_until.is_none()
        {
            out.push((n.id, n.title, similarity));
        }
    }
    Ok(out)
}
