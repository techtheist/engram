//! Binding by the client's MCP roots.

use crate::*;

/// The roots half of the bridge's one bounded brain: which project
/// root the session is bound to, and how to move it.
pub(crate) struct RootsBinding {
    pub(crate) fallback_root: std::path::PathBuf,
    pub(crate) resolve: RootResolver,
    pub(crate) state: Arc<BridgeState>,
    /// Canonicalized root of the current binding.
    pub(crate) current_root: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// Serializes bind/rebind so two triggers can't interleave connects.
    pub(crate) bind_gate: tokio::sync::Mutex<()>,
    /// Flipped by the client's `notifications/initialized` — the earliest
    /// spec-legal moment to send our roots/list request.
    pub(crate) initialized: tokio::sync::watch::Sender<bool>,
}

/// How long to wait for a roots/list answer before falling back to cwd (5 s).
/// `ENGRAM_ROOTS_TIMEOUT_SECS` overrides it for tests on slow CI runners.
pub(crate) fn roots_timeout() -> std::time::Duration {
    std::env::var("ENGRAM_ROOTS_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs,
        )
}

impl RootsBinding {
    /// Ask the client for its roots; the first `file://` root wins. None on
    /// timeout, error, or an empty/non-file answer — the caller falls back.
    pub(crate) async fn query_roots(
        &self,
        peer: &rmcp::service::Peer<rmcp::RoleServer>,
    ) -> Option<std::path::PathBuf> {
        let window = roots_timeout();
        #[allow(deprecated)] // SEP-2577 deprecates roots; clients still speak it
        match tokio::time::timeout(window, peer.list_roots()).await {
            Ok(Ok(listed)) => {
                let root = listed.roots.iter().find_map(|r| file_uri_to_path(&r.uri));
                if root.is_none() {
                    tracing::info!("client roots carry no file:// root — falling back to cwd");
                }
                root
            }
            Ok(Err(e)) => {
                tracing::warn!("roots/list failed ({e}) — falling back to cwd");
                None
            }
            Err(_) => {
                tracing::warn!("roots/list unanswered after {window:?} — falling back to cwd");
                None
            }
        }
    }

    /// Bind the session to `root`: resolve its upstream URL (registering the
    /// project with the core), open the new session, retire the old one.
    /// No-op when already bound there.
    pub(crate) async fn bind_to(
        &self,
        root: std::path::PathBuf,
        from_roots: bool,
    ) -> anyhow::Result<()> {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let _gate = self.bind_gate.lock().await;
        if self.current_root.locked().as_deref() == Some(&root) {
            return Ok(());
        }
        let resolve = self.resolve.clone();
        let resolving = root.clone();
        let target = tokio::task::spawn_blocking(move || resolve(resolving)).await??;
        // The resolver only knows whether the root hosts a project; the
        // bridge knows whether the client named it.
        let bound_by = match target.bound_by {
            BoundBy::Cwd if from_roots => BoundBy::Roots,
            other => other,
        };
        self.state
            .connect(&target.url, Some(&target.lease_root), bound_by)
            .await?;
        // A reconnect re-resolves this same root the same way.
        let (resolve, bound_root) = (self.resolve.clone(), root.clone());
        *self.state.rebind.locked() = Some(Arc::new(move || {
            let mut target = resolve(bound_root.clone())?;
            if from_roots && target.bound_by == BoundBy::Cwd {
                target.bound_by = BoundBy::Roots;
            }
            Ok(target)
        }));
        tracing::info!(
            "bridge bound to {} ({}) by {}",
            target.lease_root,
            target.url,
            bound_by.as_str()
        );
        *self.current_root.locked() = Some(root);
        Ok(())
    }

    /// The initial binding, run once the stdio handshake is done: roots when
    /// the client advertises them (after a short grace for its initialized
    /// notification), cwd otherwise.
    pub(crate) async fn initial_bind(
        self: Arc<Self>,
        peer: rmcp::service::Peer<rmcp::RoleServer>,
        client_has_roots: bool,
    ) -> anyhow::Result<()> {
        let mut from_roots = false;
        let root = if client_has_roots {
            let mut rx = self.initialized.subscribe();
            let initialized = async {
                loop {
                    if *rx.borrow_and_update() {
                        return;
                    }
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
            };
            // A client that skips initialized still gets asked after the
            // grace — the roots timeout bounds the total either way.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), initialized).await;
            if let Some(root) = self.query_roots(&peer).await {
                tracing::info!("binding by client roots: {}", root.display());
                from_roots = true;
                root
            } else {
                tracing::info!("binding by cwd fallback: {}", self.fallback_root.display());
                self.fallback_root.clone()
            }
        } else {
            tracing::info!(
                "client advertises no roots — binding by cwd fallback: {}",
                self.fallback_root.display()
            );
            self.fallback_root.clone()
        };
        self.bind_to(root, from_roots).await
    }

    /// roots/list_changed: re-ask, rebind when the project changed. A failed
    /// rebind keeps the current binding — better a stale project than a dead
    /// session — and logs why.
    pub(crate) async fn rebind_from(self: Arc<Self>, peer: &rmcp::service::Peer<rmcp::RoleServer>) {
        let answered = self.query_roots(peer).await;
        let from_roots = answered.is_some();
        let root = answered.unwrap_or_else(|| self.fallback_root.clone());
        if let Err(e) = self.bind_to(root, from_roots).await {
            tracing::warn!(
                "rebind after roots/list_changed failed — keeping the current project: {e:#}"
            );
        }
    }
}

/// `file:///Users/x/repo` → `/Users/x/repo`. Tolerates an authority
/// (`file://localhost/…`) and percent-encoding; anything else is None.
pub(crate) fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(i) if &rest[..i] == "localhost" => &rest[i..],
        _ => return None,
    };
    let decoded = percent_decode(path);
    #[cfg(windows)]
    let decoded = {
        // file:///C:/dir arrives as /C:/dir — drop the leading slash.
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            decoded[1..].to_string()
        } else {
            decoded
        }
    };
    Some(std::path::PathBuf::from(decoded))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
