//! The stdio bridge: a light process that binds a client session to a project and
//! relays it to the machine core, following the core through stalls and restarts.

use crate::*;

mod core_link;
mod gate;
mod roots;

pub(crate) use core_link::*;
pub(crate) use gate::*;
pub(crate) use roots::*;

/// How `serve_stdio_bridge` finds its upstream.
pub enum BridgeTarget {
    /// Explicit `--db`: one fixed upstream, resolved after the stdio handshake is
    /// answered (a slow core boot must never delay initialize). Roots are ignored;
    /// the proxy forwards verbatim.
    Fixed {
        /// Maps the fixed db to its upstream MCP target — spawning the
        /// machine core on the way when none runs. Blocking is fine — the
        /// bridge calls it through `spawn_blocking`, after serving.
        resolve: FixedResolver,
    },
    /// No `--db`: bind by the client's MCP roots, falling back to
    /// `fallback_root` (the bridge's cwd) when the client doesn't advertise
    /// the capability, answers with no `file://` root, or doesn't answer
    /// within the roots timeout.
    Roots {
        fallback_root: std::path::PathBuf,
        resolve: RootResolver,
    },
}

/// Where a resolved root's session connects, and the root its census lease
/// announces. When the root can't host a project, `url` points at the home
/// graph and `lease_root` at the engram home dir.
pub struct ResolvedTarget {
    pub url: String,
    pub lease_root: String,
    /// The ladder rung the resolver landed on (issue #11): `Cwd` when the
    /// root itself hosts the project (the bridge upgrades it to `Roots`
    /// when the client named that root), `DefaultProject`, `Home`, or `Db`
    /// for the fixed shape. Relayed to the core in the upstream initialize.
    pub bound_by: BoundBy,
}

/// Maps a project root to the upstream MCP target serving it (registering
/// the project with the machine core on the way). Blocking is fine — the
/// bridge calls it through `spawn_blocking`.
pub type RootResolver =
    Arc<dyn Fn(std::path::PathBuf) -> anyhow::Result<ResolvedTarget> + Send + Sync>;

/// Maps an explicit `--db` to the upstream MCP target serving it (spawning
/// the machine core on the way when none runs). Blocking is fine — the
/// bridge calls it through `spawn_blocking`, after the stdio transport is
/// already answering the handshake.
pub type FixedResolver = Arc<dyn Fn() -> anyhow::Result<ResolvedTarget> + Send + Sync>;

/// What the machine core is doing while it starts, read from outside it (the
/// core listens only after its models load) — e.g. which local models are
/// still downloading. `None` when there is nothing more specific to say than
/// "starting". Called on the bridge's async side: keep it to cheap disk reads.
pub type StartupProbe = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// How long a tool call or tool listing waits for the first binding before
/// the bridge answers it itself (0.9.11; the user's number: 15 s — under the
/// 20 s preflight timeout that read the old hold as a hang). Overridable for
/// tests via `ENGRAM_BRIDGE_GRACE_SECS`.
pub const BRIDGE_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

pub(crate) fn bridge_grace() -> std::time::Duration {
    std::env::var("ENGRAM_BRIDGE_GRACE_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map_or(BRIDGE_GRACE, std::time::Duration::from_secs_f64)
}

/// One live upstream MCP session. Generations order rebinds: a watcher or
/// heartbeat that saw generation N stays quiet when the current one moved on.
pub(crate) struct Upstream {
    pub(crate) peer: rmcp::service::Peer<rmcp::RoleClient>,
    pub(crate) generation: u64,
    pub(crate) cancel: Option<rmcp::service::RunningServiceCancellationToken>,
}

/// The bridge's census identity: where the core's lease API lives and which
/// project root this bridge currently serves. Registration is best-effort
/// observability — an older core without `/clients` (or a failed POST) never
/// blocks bridging.
#[derive(Clone)]
pub(crate) struct LeaseState {
    /// The core's origin, e.g. `http://127.0.0.1:8787`.
    pub(crate) base_url: String,
    /// Absolute project root the bridge is bound to.
    pub(crate) root: String,
    pub(crate) lease_id: Option<String>,
}

/// Everything the proxy, the heartbeat, and the binder share.
pub(crate) struct BridgeState {
    pub(crate) http: reqwest::Client,
    pub(crate) slot: std::sync::Mutex<Option<Upstream>>,
    /// Latest bound generation (0 = not bound yet); `peer_when_bound` waits
    /// on it so early requests can't race the first binding.
    pub(crate) bound: tokio::sync::watch::Sender<u64>,
    pub(crate) lease: std::sync::Mutex<Option<LeaseState>>,
    /// `clientInfo.name` from the stdio client's initialize, once seen —
    /// carried on census registrations and renewals so the core can say
    /// which MCP client (Claude Code, mcp-go/Windsurf, …) holds each lease.
    pub(crate) client_name: std::sync::Mutex<Option<String>>,
    /// Fatal bridge errors (core closed the current session, binding failed)
    /// funnel here; the main select exits on the first one.
    pub(crate) exit: tokio::sync::mpsc::Sender<anyhow::Error>,
    /// Why binding failed for good. Held client requests are answered with it, so
    /// the client sees the reason instead of "Failed to initialize server".
    pub(crate) failed: std::sync::Mutex<Option<String>>,
    /// See [`StartupProbe`] — what to tell a call answered before binding.
    pub(crate) startup: Option<StartupProbe>,
    /// See [`BRIDGE_GRACE`].
    pub(crate) grace: std::time::Duration,
    /// Generations only ever grow, even across a dropped upstream — a stale
    /// watcher must never match a newer session.
    pub(crate) next_generation: std::sync::atomic::AtomicU64,
    /// The current upstream's watcher reports here when the core closes it.
    pub(crate) lost: tokio::sync::mpsc::UnboundedSender<u64>,
    /// How the current binding was resolved, for [`Self::reconnect`].
    pub(crate) rebind: std::sync::Mutex<Option<FixedResolver>>,
    /// The current core's origin, e.g. `http://127.0.0.1:8787`.
    pub(crate) origin: std::sync::Mutex<Option<String>>,
    /// The pid the core's `/health` named when this session connected —
    /// how a reconnect tells a restarted core from a busy one.
    pub(crate) core_pid: std::sync::Mutex<Option<u64>>,
    /// The stdio client, for `notifications/tools/list_changed`.
    pub(crate) client: std::sync::Mutex<Option<rmcp::service::Peer<rmcp::RoleServer>>>,
}

impl BridgeState {
    /// A bridge with nothing bound yet, and the receiver its upstream
    /// watchers report closed sessions to (the supervisor's).
    pub(crate) fn new(
        http: reqwest::Client,
        exit: tokio::sync::mpsc::Sender<anyhow::Error>,
        startup: Option<StartupProbe>,
        grace: std::time::Duration,
    ) -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<u64>) {
        let (bound, _) = tokio::sync::watch::channel(0u64);
        let (lost, lost_rx) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::new(Self {
            http,
            slot: std::sync::Mutex::new(None),
            bound,
            lease: std::sync::Mutex::new(None),
            client_name: std::sync::Mutex::new(None),
            exit,
            failed: std::sync::Mutex::new(None),
            startup,
            grace,
            next_generation: std::sync::atomic::AtomicU64::new(0),
            lost,
            rebind: std::sync::Mutex::new(None),
            origin: std::sync::Mutex::new(None),
            core_pid: std::sync::Mutex::new(None),
            client: std::sync::Mutex::new(None),
        });
        (state, lost_rx)
    }

    /// The current upstream peer if the first binding lands within the
    /// grace, `None` if it is still in flight — the caller answers itself.
    /// A binding that failed answers with its error, as
    /// [`peer_when_bound`] does.
    pub(crate) async fn peer_within_grace(
        &self,
    ) -> Result<Option<rmcp::service::Peer<rmcp::RoleClient>>, ErrorData> {
        if let Some((peer, _)) = self.current_peer() {
            return Ok(Some(peer));
        }
        self.wait_bound(self.grace).await;
        if let Some((peer, _)) = self.current_peer() {
            return Ok(Some(peer));
        }
        if let Some(why) = self.failed.locked().clone() {
            return Err(ErrorData::internal_error(why, None));
        }
        Ok(None)
    }

    /// The tool result for a call that arrived before the core was up.
    pub(crate) fn starting_note(&self) -> String {
        let detail = self
            .startup
            .as_ref()
            .and_then(|probe| probe())
            .map(|d| format!(" {d}"))
            .unwrap_or_default();
        format!(
            "Engram's core is still starting, so this tool call was not run and nothing was \
             written.{detail} Retry the call in a minute."
        )
    }

    pub(crate) fn current_peer(&self) -> Option<(rmcp::service::Peer<rmcp::RoleClient>, u64)> {
        self.slot
            .locked()
            .as_ref()
            .map(|u| (u.peer.clone(), u.generation))
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.slot.locked().as_ref().map_or(0, |u| u.generation)
    }

    /// The current upstream peer, waiting out an in-flight (re)binding. The cap is
    /// slack over the worst legitimate bind (first-run provisioning included); a
    /// failed bind wakes every waiter at once via [`fail`].
    pub(crate) async fn peer_when_bound(
        &self,
    ) -> Result<rmcp::service::Peer<rmcp::RoleClient>, ErrorData> {
        if let Some((peer, _)) = self.current_peer() {
            return Ok(peer);
        }
        self.wait_bound(std::time::Duration::from_secs(210)).await;
        self.current_peer().map(|(peer, _)| peer).ok_or_else(|| {
            let why = self.failed.locked().clone().unwrap_or_else(|| {
                "the engram bridge has no project binding yet — the core may be unreachable \
                 (see .engram/mcp.log, or ~/.engram/mcp.log when the launch cwd is unwritable)"
                    .into()
            });
            ErrorData::internal_error(why, None)
        })
    }

    /// Wait, at most `limit`, until there is an upstream to use or the
    /// binding failed for good — the first binding and a reconnect alike.
    pub(crate) async fn wait_bound(&self, limit: std::time::Duration) {
        let mut rx = self.bound.subscribe();
        let bound = async {
            loop {
                if self.current_peer().is_some() || self.failed.locked().is_some() {
                    return;
                }
                if rx.changed().await.is_err() {
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(limit, bound).await;
    }

    /// Record a fatal binding failure and wake everything waiting in
    /// [`peer_when_bound`] — held requests answer with a JSON-RPC error
    /// carrying `why` instead of dying unanswered when the bridge exits.
    pub(crate) fn fail(&self, why: String) {
        *self.failed.locked() = Some(why);
        // u64::MAX satisfies the "bound moved" wake without ever being a
        // real generation; the woken waiter finds no peer and reads
        // `failed` for the message.
        let _ = self.bound.send(u64::MAX);
    }

    /// Open a new upstream session to `url` and make it current. The old session
    /// is cancelled (its transport DELETEs it server-side) and the census lease is
    /// renewed in place.
    pub(crate) async fn connect(
        self: &Arc<Self>,
        url: &str,
        lease_root: Option<&str>,
        bound_by: BoundBy,
    ) -> anyhow::Result<rmcp::model::ServerInfo> {
        let transport = rmcp::transport::StreamableHttpClientTransport::with_client(
            self.http.clone(),
            rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                url.to_string(),
            ),
        );
        // Upstream, the bridge names itself as the client it fronts and carries its
        // ladder rung in `description` (issue #11).
        let end_client = self.client_name.locked().clone();
        let mut client_info = rmcp::model::ClientInfo::default();
        client_info.client_info = Implementation::new(
            end_client.unwrap_or_else(|| "engram-bridge".into()),
            env!("CARGO_PKG_VERSION"),
        )
        .with_title("via engram-bridge")
        .with_description(bound_by.announce());
        let client = client_info.serve(transport).await?;
        let info = client
            .peer()
            .peer_info()
            .map(|i| (*i).clone())
            .ok_or_else(|| anyhow::anyhow!("daemon MCP handshake returned no server info"))?;
        let peer = client.peer().clone();
        let cancel = client.cancellation_token();
        let (old, generation) = {
            let mut slot = self.slot.locked();
            let generation = self
                .next_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let old = slot.replace(Upstream {
                peer,
                generation,
                cancel: Some(cancel),
            });
            (old, generation)
        };
        // An upstream that ends while current means the core closed the session:
        // reconnect now. A cancelled predecessor sees it is stale and stays quiet.
        let state = self.clone();
        tokio::spawn(async move {
            let _ = client.waiting().await;
            if state.current_generation() == generation {
                let _ = state.lost.send(generation);
            }
        });
        *self.origin.locked() = origin_of(url);
        *self.core_pid.locked() = self.core_health().await;
        if let Some(mut old) = old
            && let Some(token) = old.cancel.take()
        {
            token.cancel();
        }
        if let Some(root) = lease_root
            && let Some(base_url) = origin_of(url)
        {
            self.update_lease(base_url, root.to_string()).await;
        }
        let _ = self.bound.send(generation);
        Ok(info)
    }

    /// Point the census lease at (base_url, root). An existing lease on the
    /// same core is renewed with the new root — same lease_id, the census
    /// row just moves; a lease on a different core (repo-local daemon vs
    /// machine core) is withdrawn and re-registered there.
    pub(crate) async fn update_lease(&self, base_url: String, root: String) {
        let prev = self.lease.locked().clone();
        let name = self.client_name.locked().clone();
        let name = name.as_deref();
        let lease_id = match prev {
            Some(prev) if prev.base_url == base_url => match prev.lease_id {
                Some(id) if ping_lease(&self.http, &base_url, &id, Some(&root), name).await => {
                    Some(id)
                }
                _ => register_lease(&self.http, &base_url, &root, name).await,
            },
            Some(prev) => {
                if let Some(id) = prev.lease_id {
                    delete_lease(&self.http, &prev.base_url, &id).await;
                }
                register_lease(&self.http, &base_url, &root, name).await
            }
            None => register_lease(&self.http, &base_url, &root, name).await,
        };
        *self.lease.locked() = Some(LeaseState {
            base_url,
            root,
            lease_id,
        });
    }

    /// Follow the core: ping upstream every beat (the HTTP transport
    /// auto-reconnects, so death never shows as a closed connection). A missed
    /// ping is checked against `/health`: a core that answers is busy, and the
    /// session waits up to [`stall_patience`] (Problem 00fkvifk0v8a); otherwise
    /// [`Self::reconnect`]. Lease renewals ride the beat. Returns the fatal error.
    pub(crate) async fn supervise(
        self: Arc<Self>,
        mut lost: tokio::sync::mpsc::UnboundedReceiver<u64>,
    ) -> anyhow::Error {
        let beat = std::time::Duration::from_secs(heartbeat_secs());
        let mut last_beat = std::time::SystemTime::now();
        let mut misses = 0u32;
        let mut first_miss: Option<std::time::Instant> = None;
        loop {
            let closed = tokio::select! {
                _ = tokio::time::sleep(beat) => None,
                Some(generation) = lost.recv() => Some(generation),
            };
            // Wall clock, not the monotonic one: a sleeping Mac stops the
            // monotonic clock, so only the wall clock shows the gap.
            let now = std::time::SystemTime::now();
            let gap = now.duration_since(last_beat).unwrap_or_default();
            last_beat = now;
            if let Some(slept) = slept_through(gap, beat) {
                tracing::info!(
                    "{}s since the last heartbeat (every {}s) — the machine likely slept; \
                     re-checking the core",
                    slept.as_secs(),
                    beat.as_secs()
                );
                misses = 0;
                first_miss = None;
            }
            if let Some(generation) = closed {
                if generation != self.current_generation() {
                    continue; // a rebind retired that session — not a loss
                }
                self.drop_upstream(generation);
                if let Err(e) = self.reconnect("the core closed the session").await {
                    return e;
                }
                misses = 0;
                first_miss = None;
                continue;
            }
            let Some((peer, generation)) = self.current_peer() else {
                // Not bound yet — the initial binding is still in flight.
                continue;
            };
            match ping_core(&peer).await {
                Ok(took) => {
                    if took >= SLOW_PING {
                        tracing::warn!("the core took {:.1}s to answer a ping", took.as_secs_f64());
                    }
                    if let Some(since) = first_miss.take() {
                        tracing::info!(
                            "the core answers pings again after {misses} miss(es) over {:.0}s",
                            since.elapsed().as_secs_f64()
                        );
                    }
                    misses = 0;
                    self.renew_lease().await;
                }
                Err(why) => {
                    if self.current_generation() != generation {
                        continue; // a rebind cut this ping mid-flight — not a loss
                    }
                    misses += 1;
                    let since = *first_miss.get_or_insert_with(std::time::Instant::now);
                    let healthy = self.core_health().await.is_some();
                    tracing::warn!(
                        "core ping {why} (miss {misses}, {:.0}s since the first); /health {}",
                        since.elapsed().as_secs_f64(),
                        if healthy {
                            "answers — the core is busy, not gone"
                        } else {
                            "does not answer"
                        }
                    );
                    if healthy && since.elapsed() < stall_patience() {
                        continue;
                    }
                    let reason = if healthy {
                        format!(
                            "the core's /health answers but this session missed pings for {}s",
                            since.elapsed().as_secs()
                        )
                    } else {
                        "the core stopped answering".to_string()
                    };
                    if let Err(e) = self.reconnect(&reason).await {
                        return e;
                    }
                    misses = 0;
                    first_miss = None;
                }
            }
        }
    }

    /// Get back to a working upstream, or give up. Never spawns a core (`stop`
    /// stays a stop): waits up to [`reconnect_window`] for `/health`, keeps the
    /// session if it still pings, else re-resolves, reconnects and tells the client
    /// the tool list may have changed.
    pub(crate) async fn reconnect(self: &Arc<Self>, reason: &str) -> Result<(), anyhow::Error> {
        let window = reconnect_window();
        let start = std::time::Instant::now();
        tracing::warn!(
            "{reason} — waiting up to {}s for a core to reconnect to",
            window.as_secs()
        );
        let mut attempts = 0u32;
        loop {
            if let Some(pid) = self.core_health().await {
                if let Some((peer, _)) = self.current_peer()
                    && ping_core(&peer).await.is_ok()
                {
                    // The HTTP transport re-initializes a session a new core
                    // answers 404 for, so a kept session can sit on a new
                    // core — maybe a new version. Only the pid tells.
                    let was = self.core_pid.locked().replace(pid);
                    if was.is_some_and(|was| was != pid) {
                        tracing::info!(
                            "reconnected to a new core (pid {pid}) after {:.1}s — the session \
                             re-initialized on it",
                            start.elapsed().as_secs_f64()
                        );
                        self.announce_tools_changed().await;
                    } else {
                        tracing::info!(
                            "the core answers again after {:.1}s — keeping this session",
                            start.elapsed().as_secs_f64()
                        );
                    }
                    return Ok(());
                }
                attempts += 1;
                match self.reopen().await {
                    Ok(info) => {
                        tracing::info!(
                            "reconnected to core v{} after {:.1}s ({attempts} attempt(s))",
                            info.server_info.version,
                            start.elapsed().as_secs_f64()
                        );
                        self.announce_tools_changed().await;
                        return Ok(());
                    }
                    Err(e) => tracing::warn!("reconnect attempt {attempts} failed: {e:#}"),
                }
            }
            if start.elapsed() >= window {
                let why = format!(
                    "the engram core went away ({reason}) and none answered for {}s — bridge \
                     exiting",
                    window.as_secs()
                );
                self.fail(format!("engram bridge: {why}"));
                return Err(anyhow::anyhow!(why));
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    /// Re-resolve the stored target and open a new session to it.
    pub(crate) async fn reopen(self: &Arc<Self>) -> anyhow::Result<rmcp::model::ServerInfo> {
        let resolve = self
            .rebind
            .locked()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no binding to reconnect"))?;
        let target = tokio::task::spawn_blocking(move || resolve()).await??;
        self.connect(&target.url, Some(&target.lease_root), target.bound_by)
            .await
    }

    /// Forget a session the core closed, so new client requests wait for the
    /// reconnect in [`Self::peer_when_bound`] instead of failing on it.
    pub(crate) fn drop_upstream(&self, generation: u64) {
        let mut slot = self.slot.locked();
        if slot.as_ref().is_some_and(|u| u.generation == generation) {
            *slot = None;
        }
    }

    /// `GET /health` on the current core's origin — a second opinion that
    /// shares nothing with the MCP session. The answering core's pid when it
    /// answers (0 from a core older than 0.10.4, which doesn't say).
    pub(crate) async fn core_health(&self) -> Option<u64> {
        let origin = self.origin.locked().clone()?;
        let reply = self
            .http
            .get(format!("{origin}/health"))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success())?;
        let body: serde_json::Value = reply.json().await.unwrap_or_default();
        Some(body["pid"].as_u64().unwrap_or(0))
    }

    pub(crate) async fn announce_tools_changed(&self) {
        let client = self.client.locked().clone();
        if let Some(client) = client {
            let _ = client.notify_tool_list_changed().await;
        }
    }

    /// Renew the census lease. Best-effort: a 404 means the lease expired
    /// (or the core restarted its table) — re-register instead of silently
    /// vanishing from the census.
    pub(crate) async fn renew_lease(&self) {
        let lease = self.lease.locked().clone();
        let Some(l) = lease else { return };
        let name = self.client_name.locked().clone();
        let renewed = match &l.lease_id {
            Some(id) => {
                ping_lease(&self.http, &l.base_url, id, Some(&l.root), name.as_deref()).await
            }
            None => false,
        };
        if renewed {
            return;
        }
        let id = register_lease(&self.http, &l.base_url, &l.root, name.as_deref()).await;
        let mut slot = self.lease.locked();
        match slot.as_mut() {
            // Only fill the row we renewed for — a rebind that raced this
            // re-register owns the slot now.
            Some(cur) if cur.base_url == l.base_url && cur.root == l.root => {
                cur.lease_id = id;
            }
            _ => {
                if let Some(id) = id {
                    let http = self.http.clone();
                    let base = l.base_url.clone();
                    tokio::spawn(async move {
                        delete_lease(&http, &base, &id).await;
                    });
                }
            }
        }
    }
}

/// Bridge stdio to a core's MCP endpoint until either side goes away. The
/// handshake is served first (clients kill a server silent for ~60 s); `Fixed`
/// resolves in the background, `Roots` binds from the client's roots and
/// follows changes. A failed resolution answers held requests before exiting.
pub async fn serve_stdio_bridge(target: BridgeTarget) -> anyhow::Result<()> {
    serve_stdio_bridge_with(target, None).await
}

/// [`serve_stdio_bridge`] with a [`StartupProbe`] for calls answered before
/// the first binding.
pub async fn serve_stdio_bridge_with(
    target: BridgeTarget,
    startup: Option<StartupProbe>,
) -> anyhow::Result<()> {
    // The core is on loopback; ignore HTTP(S)_PROXY, which would route it through
    // a corporate proxy (issue #2).
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| anyhow::anyhow!("building the bridge HTTP client: {e}"))?;
    let (exit, mut exit_rx) = tokio::sync::mpsc::channel::<anyhow::Error>(4);
    let (state, lost_rx) = BridgeState::new(http, exit, startup, bridge_grace());
    // In both shapes the upstream doesn't exist yet when the stdio client's
    // initialize arrives (handshake-first), so it is answered from this
    // crate's own contract — identical to what any 0.8.x core negotiates,
    // because both sides construct it here.
    let (roots, fixed) = match target {
        BridgeTarget::Fixed { resolve } => (None, Some(resolve)),
        BridgeTarget::Roots {
            fallback_root,
            resolve,
        } => {
            let (initialized, _) = tokio::sync::watch::channel(false);
            let binding = Arc::new(RootsBinding {
                fallback_root,
                resolve,
                state: state.clone(),
                current_root: std::sync::Mutex::new(None),
                bind_gate: tokio::sync::Mutex::new(()),
                initialized,
            });
            (Some(binding), None)
        }
    };
    let info = engram_server_info();
    let proxy = Passthrough {
        state: state.clone(),
        info,
        roots: roots.clone(),
    };
    let stdio = rmcp::transport::IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        rmcp::transport::io::stdio(),
    );
    let service = proxy
        .serve(PreInitGate {
            inner: stdio,
            open: false,
        })
        .await?;
    *state.client.locked() = Some(service.peer().clone());
    if let Some(resolve) = fixed {
        *state.rebind.locked() = Some(resolve.clone());
        // The Fixed connect runs AFTER the transport is serving, so the
        // handshake answers while the core comes up (a first-run provision
        // can take minutes); early tool calls hold in `peer_when_bound`.
        let fatal = state.exit.clone();
        let fail_state = state.clone();
        tokio::spawn(async move {
            let resolved = tokio::task::spawn_blocking(move || resolve())
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r);
            let connected = match resolved {
                Ok(t) => fail_state
                    .connect(&t.url, Some(&t.lease_root), t.bound_by)
                    .await
                    .map(|_| ()),
                Err(e) => Err(e),
            };
            if let Err(e) = connected {
                let e = e.context("connecting the bridge to its core");
                // Answer every held client request with the real reason
                // before exiting — a silent close is "Failed to initialize
                // server" with zero forensics (the Windsurf field trace).
                fail_state.fail(format!("engram bridge: {e:#}"));
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = fatal.send(e).await;
            }
        });
    }
    if let Some(binding) = roots {
        let peer = service.peer().clone();
        #[allow(deprecated)] // SEP-2577 deprecates roots; clients still speak it
        let client_has_roots = peer
            .peer_info()
            .is_some_and(|i| i.capabilities.roots.is_some());
        let fatal = state.exit.clone();
        let fail_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = binding.initial_bind(peer, client_has_roots).await {
                let e = e.context("binding the bridge to a project");
                // Answer every held client request with the real reason
                // before exiting — a silent close is "Failed to initialize
                // server" with zero forensics (the Windsurf field trace).
                fail_state.fail(format!("engram bridge: {e:#}"));
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = fatal.send(e).await;
            }
        });
    }
    // Whichever side ends first ends the bridge — stdio closing is a normal
    // disconnect; a failed binding, or a core that didn't come back within
    // the reconnect window, means lingering would only strand the client.
    let result = tokio::select! {
        served = service.waiting() => served.map(|_| ()).map_err(anyhow::Error::from),
        Some(err) = exit_rx.recv() => Err(err),
        err = state.clone().supervise(lost_rx) => Err(err),
    };
    // Clean exit withdraws the lease instead of letting it lapse.
    let parting = state.lease.locked().take();
    if let Some(l) = parting
        && let Some(id) = l.lease_id
    {
        delete_lease(&state.http, &l.base_url, &id).await;
    }
    result
}
