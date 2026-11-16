//! Keyword search helpers: tokenizing, snippets and highlights.

use super::*;

/// TepinDB creates collections lazily on first insert, so a purposed-but-
/// empty collection errors `collection_not_found` on reads — for this driver
/// that simply means "no documents yet".
pub(super) fn no_collection_is_empty<T: Default>(r: tepindb::Result<T>) -> Result<T> {
    match r {
        Err(e) if e.code == "collection_not_found" => Ok(T::default()),
        other => Ok(other?),
    }
}

// ---- document mapping ----------------------------------------------------

pub(super) fn doc_suspect(doc: Value) -> Result<Suspect> {
    Ok(serde_json::from_value(doc)?)
}

// ---- keyword snippets ----------------------------------------------------

/// TepinDB's BM25 tokenization, mirrored: lowercase, split on
/// non-alphanumeric, drop tokens shorter than 2 chars.
pub(super) fn tokenize(q: &str) -> Vec<String> {
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// FTS5's `snippet()` replacement: pick the field with the most matched
/// terms, clip a ~12-word window around the first match, and mark matching
/// words with the sentinel pair the pane/MCP already understand.
pub(super) fn make_snippet(node: &Node, terms: &[String]) -> String {
    let custom = node
        .fields
        .as_ref()
        .map(|f| {
            f.values()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let fields = [
        node.title.clone(),
        node.body.clone().unwrap_or_default(),
        node.tags.join(" "),
        node.code_refs.join(" "),
        custom,
    ];
    let matches_in = |text: &str| tokenize(text).iter().filter(|t| terms.contains(t)).count();
    let best = fields
        .iter()
        .max_by_key(|f| matches_in(f))
        .filter(|f| matches_in(f) > 0);
    match best {
        Some(text) => highlight(text, terms, 12),
        None => crate::store::excerpt(node),
    }
}

pub(super) fn highlight(text: &str, terms: &[String], window: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let word_matches = |w: &str| tokenize(w).iter().any(|t| terms.contains(t));
    let first = words.iter().position(|w| word_matches(w)).unwrap_or(0);
    let start = first.saturating_sub(window / 3);
    let end = (start + window).min(words.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    for (i, word) in words[start..end].iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if word_matches(word) {
            out.push(SNIPPET_OPEN);
            out.push_str(word);
            out.push(SNIPPET_CLOSE);
        } else {
            out.push_str(word);
        }
    }
    if end < words.len() {
        out.push('…');
    }
    out
}
