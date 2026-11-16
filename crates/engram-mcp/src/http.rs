//! The daemon's streamable-HTTP `/mcp` endpoint and the bridge passthrough.

use crate::*;

// ---- daemon-hosted MCP ----

/// The daemon-hosted MCP service type, nameable so the CLI can cache one per
/// project (v0.6.2: `/projects/{id}/mcp`).
pub type McpHttpService = rmcp::transport::StreamableHttpService<
    Engram,
    rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
>;

/// The daemon's `/mcp` endpoint: MCP over streamable HTTP as a tower service
/// for the daemon router. Stateful — each connected client becomes one
/// session with its own [`Engram`] instance over the shared hub, so
/// per-session audit attribution works exactly as one stdio process did.
pub fn streamable_http_service(hub: Arc<Hub>) -> McpHttpService {
    rmcp::transport::StreamableHttpService::new(
        move || Ok(Engram::with_hub(hub.clone())),
        Arc::new(Default::default()),
        Default::default(),
    )
}

/// The per-project form (v0.6.2 machine core): sessions on this service
/// treat `selector`'s graph as the current project — an MCP bridge from repo
/// X binds to X, however the core was launched. The `project` tool param
/// still overrides per call.
pub fn streamable_http_service_for(hub: Arc<Hub>, selector: String) -> McpHttpService {
    rmcp::transport::StreamableHttpService::new(
        move || {
            Engram::for_project(hub.clone(), &selector)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))
        },
        Arc::new(Default::default()),
        Default::default(),
    )
}

/// A stdio client's passthrough to a core's per-project MCP endpoint (the core
/// owns the store file; everyone else talks HTTP). Its one decision is which
/// project to bind: fixed with `--db`, else from the client's MCP roots (cwd as
/// fallback), following `roots/list_changed` (issue #4).
pub(crate) struct Passthrough {
    pub(crate) state: Arc<BridgeState>,
    pub(crate) info: rmcp::model::ServerInfo,
    pub(crate) roots: Option<Arc<RootsBinding>>,
}

pub(crate) fn proxy_err(e: rmcp::ServiceError) -> ErrorData {
    match e {
        rmcp::ServiceError::McpError(data) => data,
        other => ErrorData::internal_error(format!("daemon bridge: {other}"), None),
    }
}

impl rmcp::Service<rmcp::RoleServer> for Passthrough {
    async fn handle_request(
        &self,
        request: rmcp::model::ClientRequest,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ServerResult, ErrorData> {
        // Initialize is answered from `info`: no upstream exists yet in either mode.
        if let rmcp::model::ClientRequest::InitializeRequest(init) = &request {
            // Remember the client's name so the lease says who bound where.
            let name = init.params.client_info.name.trim().to_string();
            if !name.is_empty() {
                *self.state.client_name.locked() = Some(name);
            }
            return Ok(rmcp::model::ServerResult::InitializeResult(
                self.info.clone(),
            ));
        }
        // Tool calls and listings wait a short grace for the binding, then get
        // answered (Insight 00dxh3ev2j1f): a first-run core downloads ~450 MB of
        // models, and silence read as a hang. A call gets "core still starting";
        // nothing runs.
        match &request {
            rmcp::model::ClientRequest::CallToolRequest(_) => {
                return match self.state.peer_within_grace().await? {
                    Some(peer) => peer.send_request(request).await.map_err(proxy_err),
                    None => Ok(rmcp::model::ServerResult::CallToolResult(
                        CallToolResult::error(vec![ContentBlock::text(self.state.starting_note())]),
                    )),
                };
            }
            rmcp::model::ClientRequest::ListToolsRequest(_) => {
                return match self.state.peer_within_grace().await? {
                    Some(peer) => peer.send_request(request).await.map_err(proxy_err),
                    None => Ok(rmcp::model::ServerResult::ListToolsResult(
                        rmcp::model::ListToolsResult::with_all_items(
                            Engram::tool_router().list_all(),
                        ),
                    )),
                };
            }
            _ => {}
        }
        // Every other early request holds until the session is bound to a
        // project, so nothing lands in the wrong graph while roots are still
        // being resolved.
        let peer = self.state.peer_when_bound().await?;
        peer.send_request(request).await.map_err(proxy_err)
    }

    async fn handle_notification(
        &self,
        notification: rmcp::model::ClientNotification,
        context: rmcp::service::NotificationContext<rmcp::RoleServer>,
    ) -> Result<(), ErrorData> {
        if let Some(roots) = &self.roots {
            match &notification {
                rmcp::model::ClientNotification::InitializedNotification(_) => {
                    // The binding trigger: roots/list is only spec-legal
                    // after this. Not replayed upstream — the upstream
                    // session runs its own handshake.
                    let _ = roots.initialized.send(true);
                    return Ok(());
                }
                rmcp::model::ClientNotification::RootsListChangedNotification(_) => {
                    // Re-ask and rebind off-thread; the notification itself
                    // has no reply to hold up.
                    let binding = roots.clone();
                    let peer = context.peer.clone();
                    tokio::spawn(async move { binding.rebind_from(&peer).await });
                    return Ok(());
                }
                _ => {}
            }
        }
        let peer = self.state.peer_when_bound().await?;
        peer.send_notification(notification)
            .await
            .map_err(proxy_err)
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        self.info.clone()
    }
}
