//! Thin axum API over `engram_core::Hub`: CRUD, hybrid search, the whole-graph
//! read the pane renders, and an SSE stream of every mutation.
//!
//! Every graph route also exists under `/projects/{id-or-name}/…`: a rewrite
//! layer strips the prefix and stashes the selector, so one handler set serves
//! both. Bare routes are the launch project; `home` is the user-level graph;
//! registry meta ops live at `/projects`.

mod error;
mod handlers;
mod params;
mod router;
mod state;

pub use error::*;
pub use handlers::config::*;
use handlers::{
    graph::*, history::*, journal::*, projects::*, review::*, search::*, system::*, tasks::*,
};
use params::*;
pub use router::*;
pub use state::*;

use engram_core::sync::MutexExt;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Json;
use axum::Router;
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use engram_core::{
    AnsweredHint, AuditOrigin, AuditPage, AuditSweep, ChangeEvent, ClaimReport, Drift, Edge,
    EdgePatch, EdgeType, Engine, Error, ExportGraph, Hub, ImportSummary, NewEdge, NewNode,
    NliAgreement, Node, NodePatch, NodeType, ProjectInfo, SuspectVerdict, SuspectView, TagStat,
    TimelineEntry, registry,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;

#[cfg(test)]
mod tests;
