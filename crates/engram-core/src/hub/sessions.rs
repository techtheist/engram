//! Per-session bindings: which project each MCP session is bound to, and how.

use super::*;

impl Hub {
    /// A live MCP session announces (or moves, on a scoped brief) its binding.
    /// `selector` is any project selector; `None` = the hub's launch project.
    /// Census truth for `/system`: after an in-session rebind the bridge's
    /// lease still shows the launch root, so readers ask here.
    pub fn session_bind(&self, session_id: &str, selector: Option<&str>) {
        let id = match selector {
            Some(sel) => match self.resolve_id(sel) {
                Ok(id) => id,
                Err(_) => return,
            },
            None => self.current.id.clone(),
        };
        let name = self.project_name(&id);
        let now = crate::store::now();
        let mut sessions = self.sessions.locked();
        let entry = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionBinding {
                session_id: session_id.to_string(),
                project: id.clone(),
                name: name.clone(),
                since: now,
                last_seen: now,
                client: None,
                bound_by: None,
            });
        entry.project = id;
        entry.name = name;
        entry.last_seen = now;
    }

    /// What a session learned about itself after binding: the end
    /// client's name and the ladder rung — census legibility for `/system`
    /// and the pane, so "which Windsurf session is still on the fallback"
    /// is a glance, not a log dig.
    pub fn session_meta(&self, session_id: &str, client: Option<String>, bound_by: &str) {
        if let Some(s) = self.sessions.locked().get_mut(session_id) {
            if client.is_some() {
                s.client = client;
            }
            s.bound_by = Some(bound_by.to_string());
            s.last_seen = crate::store::now();
        }
    }

    /// A tool call on the session — keeps its census row alive.
    pub fn session_touch(&self, session_id: &str) {
        if let Some(s) = self.sessions.locked().get_mut(session_id) {
            s.last_seen = crate::store::now();
        }
    }

    /// The session's transport closed — drop its census row.
    pub fn session_end(&self, session_id: &str) {
        self.sessions.locked().remove(session_id);
    }

    /// Live session bindings, stale ones pruned on the way out (lazy expiry,
    /// like client leases).
    pub fn sessions(&self) -> Vec<SessionBinding> {
        let now = crate::store::now();
        let mut sessions = self.sessions.locked();
        sessions.retain(|_, s| now - s.last_seen <= SESSION_BINDING_TTL_SECS);
        let mut out: Vec<SessionBinding> = sessions.values().cloned().collect();
        out.sort_by(|a, b| a.since.cmp(&b.since).then(a.session_id.cmp(&b.session_id)));
        out
    }
}
