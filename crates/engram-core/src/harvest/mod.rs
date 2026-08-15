//! The harvester (0.8.4): tails coding-assistant transcripts on this machine
//! into each project's history layer. Sweep-driven — the serve loop calls
//! [`Harvester::sweep`] on an interval; fs notifications can be layered on
//! later, the sweep is the guarantee (decision 00bgfte5usll).
//!
//! Shape per sweep: every adapter lists its transcript files; a per-file byte
//! cursor (persisted in `~/.engram/history-cursors.json`) limits parsing to
//! the appended delta; events route to a registered project by the cwd their
//! harness recorded (never by the lossy path slug); gated by that graph's
//! [`crate::config::HistoryConfig`]; then written as Session/Message nodes
//! through [`crate::Engine::add_history_node`] — no audit rows, no verdicts,
//! no trust. Messages carry `props` (harness/role/turn/event/raw_ref) and
//! chain with `next`; `in` ties them to their Session.
//!
//! Unregistered projects are skipped, not hoarded: their cursor advances and
//! the text is dropped. A disabled project's cursor does NOT advance — the
//! moment the user flips the pane toggle back on, the backlog ingests.

pub mod antigravity;
pub mod bob;
pub mod claude_code;
pub mod codex;
pub mod gemini;
pub mod kilo;
pub mod opencode;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::HistoryConfig;
use crate::history::{MESSAGE_TYPE, SESSION_TYPE, VERB_IN, VERB_NEXT};
use crate::types::{Durability, EdgeType, NewEdge, NewNode, NodeType, Source};
use crate::{Engine, Hub, Result};

// ---------------------------------------------------------------------------
// events — what adapters produce
// ---------------------------------------------------------------------------

/// Harvested harnesses — the main agent roster ([`crate::harness::AGENTS`]).
/// String forms are the stable identifiers `props.harness` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Gemini,
    Opencode,
    Kilo,
    Antigravity,
    /// IBM Bob — the IDE and BobShell share one store, so one variant.
    Bob,
}

impl Harness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::Gemini => "gemini",
            Harness::Opencode => "opencode",
            Harness::Kilo => "kilo",
            Harness::Antigravity => "antigravity",
            Harness::Bob => "bob",
        }
    }

    /// The per-graph toggle guarding this harness.
    pub fn enabled_in(&self, cfg: &HistoryConfig) -> bool {
        match self {
            Harness::ClaudeCode => cfg.harnesses.claude_code,
            Harness::Codex => cfg.harnesses.codex,
            Harness::Gemini => cfg.harnesses.gemini,
            Harness::Opencode => cfg.harnesses.opencode,
            Harness::Kilo => cfg.harnesses.kilo,
            Harness::Antigravity => cfg.harnesses.antigravity,
            Harness::Bob => cfg.harnesses.bob,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// Pointer back into the original transcript — fidelity is one seek away.
#[derive(Debug, Clone)]
pub struct RawRef {
    pub path: PathBuf,
    pub start: u64,
    pub end: u64,
}

/// One conversational turn, already filtered to prose (tool traffic and
/// sidechains dropped at parse level — decision 00bgfte7usll).
#[derive(Debug, Clone)]
pub struct HistoryEvent {
    pub harness: Harness,
    /// The cwd the harness recorded — resolved to a registered project.
    pub project_hint: PathBuf,
    /// The harness's own session identity (uuid, hash…).
    pub session_id: String,
    /// The harness's own message identity — the replay-dedupe key.
    pub event_id: String,
    pub role: Role,
    /// Unix seconds.
    pub timestamp: i64,
    pub text: String,
    pub raw_ref: RawRef,
}

/// One harness's on-disk knowledge: where transcripts live and how to parse
/// an appended delta. Adapters are pure readers — routing, gating and
/// writing are the harvester's.
pub trait HistoryAdapter: Send {
    fn harness(&self) -> Harness;
    /// Every transcript file worth polling right now.
    fn discover(&self) -> Vec<PathBuf>;
    /// Parse events appended past `cursor`; returns the events plus the new
    /// cursor (only ever advanced past COMPLETE records). A file shorter
    /// than the cursor was rotated/recreated — parse from zero.
    fn poll(&self, path: &Path, cursor: u64) -> Result<(Vec<HistoryEvent>, u64)>;
}

// ---------------------------------------------------------------------------
// cursors — the only harvester state outside the graphs
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorFile {
    #[serde(default)]
    cursors: HashMap<String, u64>,
}

fn cursor_path() -> Option<PathBuf> {
    crate::registry::engram_home().map(|d| d.join("history-cursors.json"))
}

fn load_cursors() -> HashMap<PathBuf, u64> {
    cursor_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<CursorFile>(&s).ok())
        .map(|f| {
            f.cursors
                .into_iter()
                .map(|(k, v)| (PathBuf::from(k), v))
                .collect()
        })
        .unwrap_or_default()
}

fn save_cursors(cursors: &HashMap<PathBuf, u64>) {
    let Some(path) = cursor_path() else { return };
    let file = CursorFile {
        cursors: cursors
            .iter()
            .map(|(k, v)| (k.display().to_string(), *v))
            .collect(),
    };
    let Ok(json) = serde_json::to_string_pretty(&file) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Same atomic tmp+rename discipline as the registry.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// ---------------------------------------------------------------------------
// harvester
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepStats {
    pub files: usize,
    pub events: usize,
    pub messages: usize,
    pub sessions: usize,
    /// Events dropped because their cwd matched no registered project.
    pub unrouted: usize,
}

impl SweepStats {
    pub fn wrote_anything(&self) -> bool {
        self.messages > 0 || self.sessions > 0
    }
}

/// What the harvester remembers about one live Session node between events.
struct SessionCache {
    node_id: String,
    /// Messages written so far — the next message's turn number.
    count: u64,
    last_message_id: Option<String>,
    last_ts: i64,
    /// Whether the title is real (first user message) or still a placeholder.
    titled: bool,
    /// Event ids already stored — loaded lazily, only when this session
    /// predates the process (replay after a lost cursor must not duplicate).
    seen: Option<HashSet<String>>,
    /// Dirty counters to flush into `props` at the end of the sweep.
    touched: bool,
}

pub struct Harvester {
    adapters: Vec<Box<dyn HistoryAdapter>>,
    cursors: HashMap<PathBuf, u64>,
    /// transcript path → registered project id (`None` = known-unrouted).
    routes: HashMap<PathBuf, Option<String>>,
    /// project id → harness session id → cache.
    sessions: HashMap<String, HashMap<String, SessionCache>>,
    /// project ids whose Session nodes were loaded from the store already.
    primed: HashSet<String>,
    /// Last observed [`Hub::history_epoch`] — a move means the user wiped
    /// history state and every cursor/cache here is stale.
    epoch: u64,
}

impl Harvester {
    pub fn new(adapters: Vec<Box<dyn HistoryAdapter>>) -> Self {
        Self {
            adapters,
            cursors: load_cursors(),
            routes: HashMap::new(),
            sessions: HashMap::new(),
            primed: HashSet::new(),
            epoch: 0,
        }
    }

    /// The shipped adapter set — one per roster harness, Claude Code
    /// richest first.
    pub fn first_wave() -> Self {
        Self::new(vec![
            Box::new(claude_code::ClaudeCodeAdapter::new()),
            Box::new(codex::CodexAdapter::new()),
            Box::new(gemini::GeminiAdapter::new()),
            Box::new(opencode::OpencodeAdapter::new()),
            Box::new(kilo::KiloAdapter::new()),
            Box::new(antigravity::AntigravityAdapter::new()),
            Box::new(bob::BobAdapter::new()),
        ])
    }

    /// Directories worth watching for change, deduped — the parents of every
    /// transcript the adapters can currently see (0.8.7).
    ///
    /// Directories, not files: a new session is a NEW file, and a watch on a
    /// file that does not exist yet watches nothing. Watching the parent
    /// catches both the append to an existing transcript and the creation of
    /// the next one.
    ///
    /// This is a hint for a freshness accelerator, never a guarantee. A
    /// harness that writes its first transcript into a directory tree none of
    /// these roots cover is invisible to the watcher and picked up by the next
    /// timed sweep — which is why the sweep stays the contract.
    pub fn watch_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        for a in &self.adapters {
            for file in a.discover() {
                if let Some(dir) = file.parent()
                    && !roots.iter().any(|r| r == dir)
                {
                    roots.push(dir.to_path_buf());
                }
            }
        }
        roots
    }

    /// One full pass over every adapter's transcripts. Never fails the loop:
    /// per-file errors are logged and skipped, the sweep continues.
    pub fn sweep(&mut self, hub: &Hub) -> SweepStats {
        // A moved epoch means the user wiped a history store: cursors and
        // caches are stale everywhere (cheapest correct response — the
        // seen-set dedupe makes the global re-scan idempotent for projects
        // that weren't wiped).
        let epoch = hub.history_epoch();
        if epoch != self.epoch {
            self.epoch = epoch;
            self.cursors.clear();
            self.sessions.clear();
            self.primed.clear();
            self.routes.clear();
            save_cursors(&self.cursors);
        }
        let mut stats = SweepStats::default();
        for a in 0..self.adapters.len() {
            for file in self.adapters[a].discover() {
                // Cheap pre-poll gates when we already know the file's project.
                if let Some(Some(pid)) = self.routes.get(&file)
                    && let Some(open) = self.project_gates(hub, pid, &file)
                    && !open
                {
                    continue; // disabled — cursor holds, backlog waits
                }
                let cursor = self.cursors.get(&file).copied().unwrap_or(0);
                if std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0) == cursor {
                    continue; // nothing appended
                }
                stats.files += 1;
                let (events, new_cursor) = match self.adapters[a].poll(&file, cursor) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("engram: harvest: {} unreadable: {e}", file.display());
                        continue;
                    }
                };
                stats.events += events.len();
                match self.ingest(hub, &file, events, &mut stats) {
                    // Only a fully-ingested (or deliberately dropped) delta
                    // advances the cursor; a disabled project keeps its
                    // backlog, an error retries next sweep.
                    Ok(true) => {
                        self.cursors.insert(file, new_cursor);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("engram: harvest: ingesting {}: {e}", file.display());
                    }
                }
            }
        }
        save_cursors(&self.cursors);
        // born-in provenance: with this sweep's messages in the stores,
        // parked curated notes can link to their birth exchange.
        for pid in self.sessions.keys() {
            let Ok(engine) = hub.get(pid) else { continue };
            let Ok(engine) = engine.lock() else { continue };
            if let Err(e) = engine.resolve_provenance() {
                eprintln!("engram: harvest: provenance for {pid}: {e}");
            }
        }
        stats
    }

    /// Whether `pid`'s config lets this file ingest right now — master
    /// switch plus exclude paths; the per-harness toggle is checked at
    /// ingest, where the events say which harness they are. `None` when the
    /// project can't open.
    fn project_gates(&self, hub: &Hub, pid: &str, file: &Path) -> Option<bool> {
        let engine = hub.get(pid).ok()?;
        let engine = engine.lock().ok()?;
        let h = &engine.config().history;
        let excluded = h
            .exclude_paths
            .iter()
            .any(|p| !p.trim().is_empty() && file.starts_with(p.trim()));
        Some(h.enabled && !excluded)
    }

    /// Write one file's parsed delta. Returns whether the cursor may advance.
    fn ingest(
        &mut self,
        hub: &Hub,
        file: &Path,
        events: Vec<HistoryEvent>,
        stats: &mut SweepStats,
    ) -> Result<bool> {
        let Some(first) = events.first() else {
            return Ok(true); // only filtered noise in the delta
        };
        // One transcript = one harness session = one project: route once.
        let pid = match self.routes.get(file) {
            Some(cached) => cached.clone(),
            None => {
                let resolved = crate::registry::load()
                    .resolve_root(&first.project_hint)
                    .map(|p| p.id.clone());
                self.routes.insert(file.to_path_buf(), resolved.clone());
                resolved
            }
        };
        let Some(pid) = pid else {
            stats.unrouted += events.len();
            return Ok(true); // skipped, not hoarded (plan §1)
        };

        let engine = hub.get(&pid)?;
        let mut engine = engine
            .lock()
            .map_err(|_| crate::Error::Io("engine lock poisoned".into()))?;
        let cfg = engine.config();
        let h = &cfg.history;
        let excluded = h
            .exclude_paths
            .iter()
            .any(|p| !p.trim().is_empty() && file.starts_with(p.trim()));
        if !h.enabled || excluded || !first.harness.enabled_in(h) {
            return Ok(false); // toggled off — hold the cursor for later
        }
        engine.set_audit_origin(crate::AuditOrigin::daemon());

        self.prime_sessions(&engine, &pid)?;
        let by_session = self.sessions.entry(pid.clone()).or_default();

        let mut wrote = 0usize;
        for ev in events {
            let cache = match by_session.get_mut(&ev.session_id) {
                Some(c) => c,
                None => {
                    let c = create_session(&engine, &ev, by_session)?;
                    stats.sessions += 1;
                    by_session.insert(ev.session_id.clone(), c);
                    by_session.get_mut(&ev.session_id).unwrap()
                }
            };
            // Replay guard: a session that predates this process loads its
            // stored event ids once, then membership is O(1).
            if cache.seen.is_none() {
                cache.seen = Some(load_seen(&engine, &cache.node_id));
            }
            if !cache.seen.as_mut().unwrap().insert(ev.event_id.clone()) {
                continue; // already stored (lost-cursor replay)
            }

            let node = engine.add_history_node(NewNode {
                node_type: NodeType::parse(MESSAGE_TYPE)?,
                title: truncate_words(&ev.text, 60),
                body: Some(ev.text.clone()),
                created_at: Some(ev.timestamp),
                durability: Durability::Stable,
                source: match ev.role {
                    Role::User => Source::User,
                    Role::Assistant => Source::Claude,
                },
                session_id: Some(ev.session_id.clone()),
                status: None,
                code_refs: vec![],
                tags: vec![],
                version: None,
                props: Some(message_props(&ev, cache.count)),
            })?;
            let Some(node) = node else {
                return Ok(false); // layer closed mid-sweep — retry later
            };
            engine.add_history_edge(NewEdge {
                edge_type: EdgeType::parse(VERB_IN)?,
                from_id: node.id.clone(),
                to_id: cache.node_id.clone(),
                source: Source::Claude,
                note: None,
                confidence: None,
                strength: None,
                status: None,
            })?;
            if let Some(prev) = &cache.last_message_id {
                engine.add_history_edge(NewEdge {
                    edge_type: EdgeType::parse(VERB_NEXT)?,
                    from_id: prev.clone(),
                    to_id: node.id.clone(),
                    source: Source::Claude,
                    note: None,
                    confidence: None,
                    strength: None,
                    status: None,
                })?;
            }
            // The first substantial user message names the session.
            if !cache.titled && ev.role == Role::User && ev.text.trim().len() >= 8 {
                retitle_session(&engine, &cache.node_id, &ev.text)?;
                cache.titled = true;
            }
            cache.count += 1;
            cache.last_message_id = Some(node.id);
            cache.last_ts = ev.timestamp;
            cache.touched = true;
            wrote += 1;
        }

        // Flush dirty session counters (count/ended/last_message) in one
        // write per touched session, not one per message.
        for cache in by_session.values_mut().filter(|c| c.touched) {
            flush_session(&engine, cache)?;
            cache.touched = false;
        }

        if wrote > 0 {
            stats.messages += wrote;
            engine.audit_activity(
                "history_harvested",
                Some(format!("{wrote} message(s) from {}", file.display())),
            )?;
        }
        Ok(true)
    }

    /// Load the project's existing Session nodes into the cache once per
    /// process — restart continuity for counts, chains and titles.
    fn prime_sessions(&mut self, engine: &Engine, pid: &str) -> Result<()> {
        if !self.primed.insert(pid.to_string()) {
            return Ok(());
        }
        // First touch per process: seal whatever a pre-seal daemon (or a
        // keyless period) left plaintext. No-op once the store is sealed.
        match engine.seal_history_backlog() {
            Ok(0) => {}
            Ok(n) => eprintln!("engram: harvest: sealed {n} pre-existing history node(s)"),
            Err(e) => eprintln!("engram: harvest: seal backlog: {e}"),
        }
        // And recover provenance a restart dropped: the parking lot is
        // in-memory, so recent notes without a born-in edge re-park here.
        match engine.repark_recent_provenance(24 * 3600) {
            Ok(0) | Err(_) => {}
            Ok(n) => eprintln!("engram: harvest: re-parked {n} note(s) for provenance"),
        }
        let sessions = engine
            .with_history(|s| s.nodes_by_type_active(&NodeType::parse(SESSION_TYPE)?, usize::MAX))
            .transpose()?
            .unwrap_or_default();
        let map = self.sessions.entry(pid.to_string()).or_default();
        for node in sessions {
            let Some(sid) = node.session_id.clone() else {
                continue;
            };
            let p = |k: &str| node.props.as_ref().and_then(|m| m.get(k).cloned());
            map.insert(
                sid,
                SessionCache {
                    node_id: node.id,
                    count: p("messages").and_then(|v| v.as_u64()).unwrap_or(0),
                    last_message_id: p("last_message").and_then(|v| v.as_str().map(str::to_string)),
                    last_ts: p("ended")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(node.created_at),
                    titled: p("titled").and_then(|v| v.as_bool()).unwrap_or(false),
                    seen: None,
                    touched: false,
                },
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// node shapes
// ---------------------------------------------------------------------------

fn message_props(ev: &HistoryEvent, turn: u64) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("harness".into(), ev.harness.as_str().into());
    m.insert("role".into(), ev.role.as_str().into());
    m.insert("turn".into(), turn.into());
    m.insert("event".into(), ev.event_id.clone().into());
    m.insert(
        "raw".into(),
        serde_json::json!({
            "path": ev.raw_ref.path.display().to_string(),
            "start": ev.raw_ref.start,
            "end": ev.raw_ref.end,
        }),
    );
    m
}

fn create_session(
    engine: &Engine,
    ev: &HistoryEvent,
    existing: &HashMap<String, SessionCache>,
) -> Result<SessionCache> {
    let mut props = serde_json::Map::new();
    props.insert("harness".into(), ev.harness.as_str().into());
    props.insert("path".into(), ev.raw_ref.path.display().to_string().into());
    props.insert("messages".into(), 0u64.into());
    props.insert("titled".into(), false.into());
    let title = if ev.role == Role::User && ev.text.trim().len() >= 8 {
        props.insert("titled".into(), true.into());
        truncate_words(&ev.text, 80)
    } else {
        // Placeholder until the first substantial user message.
        format!("{} session", ev.harness.as_str())
    };
    let node = engine
        .add_history_node(NewNode {
            node_type: NodeType::parse(SESSION_TYPE)?,
            title,
            body: None,
            created_at: Some(ev.timestamp),
            durability: Durability::Stable,
            source: Source::Claude,
            session_id: Some(ev.session_id.clone()),
            status: None,
            code_refs: vec![],
            tags: vec![],
            // Sessions carry the project's working version when tracking is
            // on — a plain provenance stamp, like curated nodes get.
            version: engine.current_version().ok().flatten(),
            props: Some(props.clone()),
        })?
        .ok_or_else(|| crate::Error::Io("history layer closed".into()))?;

    // Chain sessions per harness: previous latest —next→ this one.
    if let Some(prev) = existing
        .values()
        .max_by_key(|c| c.last_ts)
        .map(|c| c.node_id.clone())
    {
        engine.add_history_edge(NewEdge {
            edge_type: EdgeType::parse(VERB_NEXT)?,
            from_id: prev,
            to_id: node.id.clone(),
            source: Source::Claude,
            note: None,
            confidence: None,
            strength: None,
            status: None,
        })?;
    }
    let titled = props
        .get("titled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(SessionCache {
        node_id: node.id,
        count: 0,
        last_message_id: None,
        last_ts: ev.timestamp,
        titled,
        seen: Some(HashSet::new()),
        touched: true,
    })
}

/// Event ids already stored under a Session — walked once per pre-existing
/// session per process, then kept in memory.
fn load_seen(engine: &Engine, session_node: &str) -> HashSet<String> {
    engine
        .with_history(|s| {
            let mut seen = HashSet::new();
            for edge in s.edges_in(session_node).unwrap_or_default() {
                if edge.edge_type.as_str() != VERB_IN {
                    continue;
                }
                if let Ok(Some(msg)) = s.get_node(&edge.from_id)
                    && let Some(ev) = msg
                        .props
                        .as_ref()
                        .and_then(|p| p.get("event"))
                        .and_then(|v| v.as_str())
                {
                    seen.insert(ev.to_string());
                }
            }
            seen
        })
        .unwrap_or_default()
}

fn retitle_session(engine: &Engine, node_id: &str, text: &str) -> Result<()> {
    let Some(Some(mut node)) = engine.with_history(|s| s.get_node(node_id)).transpose()? else {
        return Ok(());
    };
    node.title = truncate_words(text, 80);
    if let Some(p) = node.props.as_mut() {
        p.insert("titled".into(), true.into());
    }
    engine.upsert_history_node(&node, true)?;
    Ok(())
}

fn flush_session(engine: &Engine, cache: &SessionCache) -> Result<()> {
    let Some(Some(mut node)) = engine
        .with_history(|s| s.get_node(&cache.node_id))
        .transpose()?
    else {
        return Ok(());
    };
    let props = node.props.get_or_insert_with(serde_json::Map::new);
    props.insert("messages".into(), cache.count.into());
    props.insert("ended".into(), cache.last_ts.into());
    if let Some(last) = &cache.last_message_id {
        props.insert("last_message".into(), last.clone().into());
    }
    engine.upsert_history_node(&node, false)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// shared parsing helpers
// ---------------------------------------------------------------------------

/// Word-boundary truncation for titles — never cuts mid-char, collapses
/// whitespace, appends an ellipsis only when something was dropped.
pub(crate) fn truncate_words(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let hard: String = flat.chars().take(max).collect();
    let cut = hard.rfind(' ').unwrap_or(hard.len());
    format!("{}…", &hard[..cut])
}

/// ISO-8601 → unix seconds, UTC ("2026-08-11T09:15:32.123Z") or ±HH:MM
/// offsets. Hand-rolled because engram-core carries no date dependency.
pub(crate) fn parse_iso8601(s: &str) -> Option<i64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm), 1970-01-01 = 0.
    let y_adj = y - i64::from(mo <= 2);
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut ts = days * 86_400 + h * 3_600 + mi * 60 + sec;
    // Offset suffix: Z, or ±HH:MM after optional fractional seconds.
    let rest = &s[19..];
    let tz = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    match tz.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => {}
        Some(sign @ (b'+' | b'-')) if tz.len() >= 6 => {
            let oh = tz.get(1..3)?.parse::<i64>().ok()?;
            let om = tz.get(4..6)?.parse::<i64>().ok()?;
            let off = oh * 3_600 + om * 60;
            ts += if *sign == b'+' { -off } else { off };
        }
        _ => return None,
    }
    Some(ts)
}
