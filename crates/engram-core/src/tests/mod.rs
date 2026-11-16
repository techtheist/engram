use crate::*;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().unwrap()
}

/// The default policy — the numbers every pre-config test ran under.
fn dp() -> crate::config::PolicyConfig {
    crate::config::PolicyConfig::default()
}

fn new_node(t: NodeType, title: &str, body: &str) -> NewNode {
    NewNode {
        node_type: t,
        title: title.to_string(),
        body: Some(body.to_string()),
        created_at: None,
        durability: Durability::Stable,
        source: Source::Agent,
        session_id: Some("s1".to_string()),
        status: None,
        code_refs: vec![],
        tags: vec![],
        version: None,
        props: None,
        fields: None,
    }
}

mod activity;
mod audit;
mod backends;
mod bridged;
mod calibration;
mod digest_scan;
mod diversity;
mod fields;
mod graph_ops;
mod harvest;
mod history_layer;
mod hub_federation;
mod import_export;
mod indexed_reads;
mod lifecycle;
mod live_config;
mod merge;
mod nli_checks;
mod sealing;
mod seams;
mod search;
mod shared_engine;
mod standing;
mod store;
mod supersession;
mod suspects;
mod tags_brief;
mod task_list;
mod tepin;
mod time;
mod title_guard;
mod tombstones;
mod trust;
mod undo;
mod versions;

use fields::*;
use graph_ops::*;
use history_layer::*;
use merge::*;
use sealing::*;
use search::*;
use shared_engine::*;
use suspects::*;
use tepin::*;
use time::*;
use title_guard::*;
