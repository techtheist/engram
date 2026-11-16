//! The MCP tools, one router per area, combined into one.

use crate::*;

mod read;
mod review;
mod tasks;
mod write;

impl Engram {
    /// Every tool this server exposes.
    pub fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::read_router() + Self::write_router() + Self::review_router() + Self::tasks_router()
    }
}
