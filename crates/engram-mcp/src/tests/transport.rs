use crate::*;
use engram_core::{FakeEmbedder, SqliteStore};
use rmcp::model::CallToolRequestParams;

/// The §7C thin-client chain, end to end and in-process: a daemon-style
/// axum server hosting /mcp, a direct streamable-HTTP client against it,
/// and a full stdio-shaped bridge (Passthrough over an in-memory duplex)
/// relaying a second client through it.
#[tokio::test]
async fn streamable_http_daemon_and_stdio_bridge_end_to_end() {
    let engine = Engine::new(
        SqliteStore::open_in_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let hub = Arc::new(Hub::single(engine));
    let app = axum::Router::new().route_service("/mcp", streamable_http_service(hub.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("http://{addr}/mcp");

    // Direct client: handshake + a real tool call against the endpoint.
    let direct = ()
        .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
            url.clone(),
        ))
        .await
        .unwrap();
    let tools = direct.peer().list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "brief"));
    assert!(tools.iter().any(|t| t.name == "add_note"));
    let noted = direct
        .peer()
        .call_tool(
            CallToolRequestParams::new("add_note").with_arguments(
                serde_json::json!({
                    "type": "Decision",
                    "title": "served over the daemon transport",
                    "durability": "stable"
                })
                .as_object()
                .cloned()
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(noted.is_error, Some(true));

    // The bridge: stdio-shaped duplex → Passthrough → the same endpoint,
    // wired through the real BridgeState::connect path (Fixed shape).
    let (exit, _exit_rx) = tokio::sync::mpsc::channel(4);
    let (state, _lost_rx) = BridgeState::new(reqwest::Client::new(), exit, None, BRIDGE_GRACE);
    let info = state.connect(&url, None, BoundBy::Route).await.unwrap();
    let proxy = Passthrough {
        state: state.clone(),
        info,
        roots: None,
    };
    let (bridge_io, client_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let server = proxy.serve(bridge_io).await.unwrap();
        let _ = server.waiting().await;
    });
    let bridged = ().serve(client_io).await.unwrap();
    assert_eq!(
        bridged.peer().peer_info().unwrap().server_info.name,
        "engram",
        "the bridge mirrors the daemon's identity"
    );
    let hits = bridged
        .peer()
        .call_tool(
            CallToolRequestParams::new("search").with_arguments(
                serde_json::json!({ "query": "daemon transport" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(hits.is_error, Some(true));
    let text = format!("{:?}", hits.content);
    assert!(
        text.contains("served over the daemon transport"),
        "a write through the direct client is visible through the bridge: {text}"
    );
}
