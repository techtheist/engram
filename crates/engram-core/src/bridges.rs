//! Bridge notes (0.11.2): a note that ties one subject to another — "the
//! export worker uses the same datastore as the import worker", "the replay
//! buffer is the old name of the ingest job". Two facts about two different
//! subjects can disagree only through such a note, and no pairwise judge can
//! see it: the subject guard rightly refuses to compare two subjects, and
//! when it does compare them an MNLI-class model calls every value clash a
//! contradiction whether or not anything ties the subjects together.
//!
//! The rule here decides which notes are bridges and which notes a bridge
//! names; the engine pairs the facts about its subjects and asks the judge
//! one question per pair — "bridge. y ⟹ x". The cue lists are plain English
//! relation words, chosen before any benchmark world was read; a bridge must
//! also name at least two subjects, so a fact that merely says "same" about
//! one thing never pairs anything. Measured on KnowledgeDrift v3 and this
//! repo's own graph (eval Finding 00ge9s5nwc1z): 0 false alarms on the real
//! graph at ~0.2 judge calls per write.
//!
//! Universal rules ("every service runs on one database") are deliberately
//! not bridges here: paired against a note's neighbourhood they caught
//! nothing and produced all the noise in the measurement.

use std::collections::HashSet;

/// Words that tie one subject to another.
const RELATION_WORDS: [&str; 26] = [
    "same",
    "share",
    "shares",
    "shared",
    "sharing",
    "identical",
    "mirror",
    "mirrors",
    "mirrored",
    "inherit",
    "inherits",
    "inherited",
    "renamed",
    "alias",
    "aka",
    "formerly",
    "copied",
    "copies",
    "equivalent",
    "reuses",
    "reused",
    "derived",
    "clone",
    "cloned",
    "twin",
    "duplicate",
];

/// Phrases that do the same across two words.
const RELATION_PHRASES: [&str; 6] = [
    "old name",
    "known as",
    "taken from",
    "takes its",
    "based on",
    "one and the same",
];

/// A rule over every member of a group…
const ALL_WORDS: [&str; 6] = [
    "every",
    "all",
    "whole",
    "entire",
    "everywhere",
    "platform-wide",
];
/// …that says the members hold one value. Together they make a universal
/// rule, which is not a pair bridge (see the module docs).
const ONE_WORDS: [&str; 8] = [
    "same",
    "single",
    "one",
    "shared",
    "common",
    "uniform",
    "standardised",
    "standardized",
];

/// Function words that never name a subject or carry a claim.
const STOP: [&str; 30] = [
    "the", "a", "an", "is", "are", "was", "were", "it", "its", "in", "on", "of", "for", "to",
    "and", "or", "that", "this", "with", "from", "by", "at", "as", "not", "no", "never", "every",
    "all", "any", "has",
];

/// How far into a title the subject may sit: "The codebase of X …" names X
/// third, and a phrase further in is a detail, not what the note is about.
const SUBJECT_WINDOW: usize = 3;

/// Two claims overlapping this much (Jaccard, the bridge's own words left
/// out) restate one another: a bridge cannot make them disagree.
const RESTATEMENT_OVERLAP: f64 = 0.7;

fn lower_words(t: &str) -> impl Iterator<Item = String> + '_ {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

fn content(w: &str) -> bool {
    w.chars().count() >= 3 && !STOP.contains(&w)
}

/// Drop parenthesised, bracketed and quoted spans: asides, not the claim.
fn strip_asides(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut close: Option<char> = None;
    for c in t.chars() {
        match close {
            Some(end) if c == end => close = None,
            Some(_) => {}
            None => match c {
                '(' => close = Some(')'),
                '[' => close = Some(']'),
                '"' => close = Some('"'),
                '`' => close = Some('`'),
                '\u{201c}' => close = Some('\u{201d}'),
                _ => out.push(c),
            },
        }
    }
    out
}

/// Does this title tie subjects together? The relation must be the claim:
/// it sits in the title's main clause ([`crate::engine::title_clause`]) and
/// outside any aside. "… — serving the same model" or "(no word shared
/// with the note)" mention sameness in passing; they tie nothing (the
/// dogfood replay, 2026-11-15: such titles were the whole of the noise).
///
/// Words are read whole, hyphenated compounds included: "shared-vocabulary
/// subjects" describes the subjects, it does not tie one to another. A
/// universal rule ("every …" + "same / one / shared …") is not a pair
/// bridge.
pub(crate) fn is_bridge(title: &str) -> bool {
    let main = strip_asides(crate::engine::title_clause(title));
    let lower = main.to_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    if has(&ALL_WORDS) && has(&ONE_WORDS) {
        return false;
    }
    has(&RELATION_WORDS) || RELATION_PHRASES.iter().any(|p| lower.contains(p))
}

/// The words through which `bridge` names the subject of `note` — empty
/// when it doesn't. The two share a subject
/// ([`crate::engine::shared_subject`]), the note names it within its first
/// [`SUBJECT_WINDOW`] content words, and the word is one the bridge itself
/// uses (the named path returns the union of both titles' names, so sharing
/// alone would let a name only the note carries through). Two notes a
/// bridge reaches through the SAME word are one subject to it, not two.
pub(crate) fn anchors(bridge: &str, note: &str) -> HashSet<String> {
    let Some(subject) = crate::engine::shared_subject(bridge, note) else {
        return HashSet::new();
    };
    let in_bridge: HashSet<String> = lower_words(crate::engine::title_clause(bridge)).collect();
    lower_words(crate::engine::title_clause(note))
        .filter(|w| content(w))
        .take(SUBJECT_WINDOW)
        .filter(|w| subject.contains(w) && in_bridge.contains(w))
        .collect()
}

/// Does `bridge` name the subject of `note`? See [`anchors`].
#[cfg(test)]
pub(crate) fn names(bridge: &str, note: &str) -> bool {
    !anchors(bridge, note).is_empty()
}

/// Do `x` and `y` say the same thing once the bridge's own words (the
/// subjects it ties) are left out? Compared on five-character stems.
pub(crate) fn restates(bridge: &str, x: &str, y: &str) -> bool {
    let own: HashSet<String> = lower_words(bridge).collect();
    let claim = |t: &str| -> HashSet<String> {
        lower_words(crate::engine::title_clause(t))
            .filter(|w| content(w) && !own.contains(w))
            .map(|w| w.chars().take(5).collect())
            .collect()
    };
    let (cx, cy) = (claim(x), claim(y));
    let union = cx.union(&cy).count();
    union > 0 && cx.intersection(&cy).count() as f64 / union as f64 >= RESTATEMENT_OVERLAP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_words_and_phrases_make_a_bridge() {
        assert!(is_bridge(
            "The Export worker uses the same datastore as the Import worker"
        ));
        assert!(is_bridge("Replay buffer is the old name of the Ingest job"));
        assert!(is_bridge("Billing takes its retry policy from Payments"));
        assert!(!is_bridge("Export worker keeps its records in PostgreSQL"));
        // Universal rules are not pair bridges.
        assert!(!is_bridge("Every service runs on one database"));
        // Sameness in passing — a later clause, an aside — ties nothing.
        assert!(!is_bridge(
            "Search runs on the GPU — serving the same model as before"
        ));
        assert!(!is_bridge(
            "Crossed phrasing (no word shared with the note) is a ceiling"
        ));
        // A hyphenated compound describes; it ties nothing.
        assert!(!is_bridge(
            "The gate barely fires on shared-vocabulary subjects"
        ));
    }

    #[test]
    fn a_bridge_names_the_subjects_its_notes_lead_with() {
        let b = "The Export worker uses the same datastore as the Import worker";
        assert!(names(b, "Export worker keeps its records in PostgreSQL"));
        assert!(names(b, "The codebase of Import worker is Rust"));
        // A subject the bridge never mentions.
        assert!(!names(b, "Billing keeps its records in MongoDB"));
        // Each subject is reached through its own word.
        assert!(
            anchors(b, "Export worker keeps its records in PostgreSQL")
                .is_disjoint(&anchors(b, "Import worker is backed by MongoDB"))
        );
        // Named, but only deep in the title: a detail, not the subject.
        assert!(!names(
            b,
            "Release notes for the dashboard mention the Export worker"
        ));
    }

    #[test]
    fn identical_claims_about_two_subjects_are_a_restatement() {
        let b = "Export worker mirrors every setting of the Import worker";
        assert!(restates(
            b,
            "Export worker drops writes when a migration half-applies",
            "Import worker drops writes when a migration half-applies",
        ));
        assert!(!restates(
            b,
            "Export worker keeps its records in PostgreSQL",
            "Import worker is backed by MongoDB",
        ));
    }
}
