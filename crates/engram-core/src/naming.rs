//! Subject names (0.11.0): what a pile of close notes is about, in two or
//! three terms, without a model. Class-based TF-IDF over titles and tags —
//! a term scores by the share of the subject's notes carrying it times how
//! rare it is in the whole graph — so "retrieval · oblique recall" wins over
//! words every note in the graph uses.

use std::collections::HashMap;

use crate::Node;

/// Terms a name shows at most.
const NAME_TERMS: usize = 3;
/// A two-word phrase beats the single words it holds by this factor.
const PHRASE_BOOST: f64 = 1.3;
/// A phrase takes over a shown word when it covers this share of the word's notes.
const PHRASE_TAKEOVER: f64 = 0.6;

const STOP: &[&str] = &[
    "the", "and", "for", "are", "its", "with", "from", "that", "this", "not", "one", "now", "when",
    "than", "into", "only", "every", "each", "was", "has", "have", "per", "can", "must", "never",
    "all", "but", "then", "their", "they", "them", "which", "what", "who", "our", "until", "after",
    "before", "over", "out", "still", "just", "new", "old", "how", "why", "does", "did", "use",
    "uses", "used", "via", "any", "more", "less", "most", "also", "both", "same", "own", "too",
    "yet", "will", "would", "should", "could", "there", "here", "where", "while", "instead",
    "without", "within", "about", "because", "between", "through", "like",
];

/// One note's terms: title words, adjacent title-word pairs, and tags (a
/// tag is keyed as words, so `process-model` meets "process model").
/// Keyed lowercase; the value is how the note spells it.
fn terms(n: &Node) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let words: Vec<&str> = n
        .title
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .map(|w| w.trim_matches(|c: char| c == '-' || c == '_'))
        .filter(|w| {
            w.chars().count() > 2
                && w.chars().any(char::is_alphabetic)
                && !STOP.contains(&w.to_lowercase().as_str())
        })
        .collect();
    for w in &words {
        out.insert(w.to_lowercase(), w.to_string());
    }
    for p in words.windows(2) {
        let shown = format!("{} {}", p[0], p[1]);
        out.insert(shown.to_lowercase(), shown);
    }
    for t in &n.tags {
        out.entry(t.replace(['-', '_'], " ").to_lowercase())
            .or_insert_with(|| t.clone());
    }
    out
}

/// Document frequency of every term across the graph's notes.
pub(crate) struct TermIndex {
    df: HashMap<String, usize>,
    docs: usize,
}

impl TermIndex {
    pub(crate) fn build<'a>(notes: impl IntoIterator<Item = &'a Node>) -> Self {
        let mut df: HashMap<String, usize> = HashMap::new();
        let mut docs = 0;
        for n in notes {
            docs += 1;
            for k in terms(n).into_keys() {
                *df.entry(k).or_default() += 1;
            }
        }
        Self { df, docs }
    }

    /// The subject's name, or None when no term is shared by two members.
    pub(crate) fn name(&self, members: &[&Node]) -> Option<String> {
        let mut count: HashMap<String, usize> = HashMap::new();
        let mut spelled: HashMap<String, HashMap<String, usize>> = HashMap::new();
        for n in members {
            for (k, shown) in terms(n) {
                *count.entry(k.clone()).or_default() += 1;
                *spelled.entry(k).or_default().entry(shown).or_default() += 1;
            }
        }
        let size = members.len().max(1) as f64;
        let docs = self.docs.max(1) as f64;
        let mut scored: Vec<(f64, String, usize)> = count
            .into_iter()
            .filter(|(_, c)| *c >= 2)
            .map(|(k, c)| {
                let df = self.df.get(&k).copied().unwrap_or(c).max(c) as f64;
                let phrase = if k.contains(' ') { PHRASE_BOOST } else { 1.0 };
                (c as f64 / size * (docs / df).ln() * phrase, k, c)
            })
            .filter(|(s, _, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));

        // Walk best first; a term that repeats a shown one (the same word,
        // a word inside a shown phrase, release/released) is skipped. A
        // phrase takes the place of the shown words it holds when it covers
        // most of their notes, or when it holds them all ("oblique" and
        // "recall" become "oblique recall"); once full, only that happens.
        let mut picked: Vec<(String, usize)> = Vec::new();
        for (_, k, c) in scored {
            let words: Vec<&str> = k.split(' ').collect();
            let hits: Vec<usize> = picked
                .iter()
                .enumerate()
                .filter(|(_, (p, _))| p.split(' ').any(|q| words.iter().any(|w| same_word(w, q))))
                .map(|(i, _)| i)
                .collect();
            let Some(&first) = hits.first() else {
                if picked.len() < NAME_TERMS {
                    picked.push((k, c));
                }
                continue;
            };
            let shorter = hits
                .iter()
                .all(|&i| picked[i].0.split(' ').count() < words.len());
            let covers = hits
                .iter()
                .all(|&i| c as f64 >= picked[i].1 as f64 * PHRASE_TAKEOVER);
            if shorter && (covers || hits.len() == words.len()) {
                for &i in hits.iter().skip(1).rev() {
                    picked.remove(i);
                }
                picked[first] = (k, c);
            }
        }
        if picked.is_empty() {
            return None;
        }
        // A deliberate spelling (TepinDB, LongMemEval) beats a sentence's
        // capital letter, which is dropped.
        let inner_caps = |s: &str| s.chars().skip(1).any(char::is_uppercase);
        let shown: Vec<String> = picked
            .into_iter()
            .map(|(k, _)| {
                let best = spelled[&k]
                    .iter()
                    .max_by(|a, b| {
                        (inner_caps(a.0), a.1)
                            .cmp(&(inner_caps(b.0), b.1))
                            .then(b.0.cmp(a.0))
                    })
                    .map(|(s, _)| s.clone())
                    .unwrap_or(k);
                if inner_caps(&best) {
                    best
                } else {
                    best.to_lowercase()
                }
            })
            .collect();
        Some(shown.join(" · "))
    }
}

/// One word, or one a suffix away from it (release / released / releases).
fn same_word(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long == short || (short.len() >= 4 && long.starts_with(short) && long.len() - short.len() <= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(title: &str, tags: &[&str]) -> Node {
        serde_json::from_value(serde_json::json!({
            "id": title, "type": "Insight", "title": title, "durability": "stable",
            "source": "claude", "created_at": 0, "tags": tags,
        }))
        .unwrap()
    }

    #[test]
    fn a_subject_is_named_by_what_its_notes_share_and_the_graph_does_not() {
        let mut graph: Vec<Node> = (0..40)
            .map(|i| note(&format!("The engine keeps note {i} in the store"), &[]))
            .collect();
        let pile = [
            note("Oblique recall drops on name-free queries", &["retrieval"]),
            note(
                "Oblique recall: hybrid fusion loses to vectors",
                &["retrieval"],
            ),
            note(
                "Three oblique recall knobs refuted in the engine",
                &["retrieval"],
            ),
            note("Released the oblique probe", &["retrieval"]),
        ];
        graph.extend(pile.iter().cloned());
        let index = TermIndex::build(&graph);
        let members: Vec<&Node> = pile.iter().collect();
        let name = index.name(&members).unwrap();
        assert!(name.contains("retrieval"), "{name}");
        assert!(name.contains("oblique recall"), "{name}");
        // "oblique" alone is inside the shown phrase; "engine" is everywhere.
        assert_eq!(name.matches("blique").count(), 1, "{name}");
        assert!(!name.contains("engine"), "{name}");
    }

    #[test]
    fn a_tag_and_its_words_are_one_term_and_suffixes_fold() {
        let mut graph: Vec<Node> = (0..30).map(|i| note(&format!("filler {i}"), &[])).collect();
        let pile = [
            note(
                "Process model: the bridge follows the core",
                &["process-model"],
            ),
            note(
                "Process model keeps one core per machine",
                &["process-model"],
            ),
            note("Bridges wait for a restarted core", &[]),
            note("The bridge pings the core", &[]),
        ];
        graph.extend(pile.iter().cloned());
        let index = TermIndex::build(&graph);
        let name = index.name(&pile.iter().collect::<Vec<_>>()).unwrap();
        assert_eq!(
            name.to_lowercase().matches("process model").count(),
            1,
            "{name}"
        );
        assert!(
            !(name.contains("bridge ") && name.contains("Bridges")),
            "{name}"
        );
    }

    #[test]
    fn nothing_shared_means_no_name() {
        let graph = vec![note("alpha beta", &[]), note("gamma delta", &[])];
        let index = TermIndex::build(&graph);
        assert_eq!(index.name(&graph.iter().collect::<Vec<_>>()), None);
    }
}
