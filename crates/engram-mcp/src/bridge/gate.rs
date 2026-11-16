//! Holds client messages until the session is initialized.

use crate::*;

/// Stdio transport gated before `initialize`: Claude Code opens with a
/// `server/discover` probe, and rmcp exits on anything but initialize or ping.
/// The gate answers other pre-init requests "method not found" and drops
/// pre-init notifications, then passes everything through.
pub(crate) struct PreInitGate<T> {
    pub(crate) inner: T,
    pub(crate) open: bool,
}

impl<T: rmcp::transport::Transport<rmcp::RoleServer>> rmcp::transport::Transport<rmcp::RoleServer>
    for PreInitGate<T>
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<rmcp::service::RxJsonRpcMessage<rmcp::RoleServer>> {
        use rmcp::model::{ClientRequest, JsonRpcMessage};
        loop {
            let msg = self.inner.receive().await?;
            if self.open {
                return Some(msg);
            }
            if let JsonRpcMessage::Request(req) = &msg {
                match &req.request {
                    ClientRequest::InitializeRequest(_) => {
                        self.open = true;
                        return Some(msg);
                    }
                    ClientRequest::PingRequest(_) => return Some(msg),
                    other => {
                        let method = pre_init_method(other);
                        tracing::info!(
                            "answered pre-initialize {method} with method-not-found (the client's \
                         version probe — the handshake goes on)"
                        );
                        let reply = JsonRpcMessage::Error(rmcp::model::JsonRpcError::new(
                            Some(req.id.clone()),
                            ErrorData::new(
                                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                                format!("{method} is not supported before initialize"),
                                None,
                            ),
                        ));
                        if self.inner.send(reply).await.is_err() {
                            return None;
                        }
                    }
                }
            } else {
                tracing::debug!("dropped a pre-initialize message");
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

pub(crate) fn pre_init_method(request: &rmcp::model::ClientRequest) -> String {
    match request {
        rmcp::model::ClientRequest::CustomRequest(c) => c.method.clone(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_owned))
            .unwrap_or_else(|| "a request".into()),
    }
}
