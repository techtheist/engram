//! Title and subject heuristics shared by the conflict and duplicate guards.

/// Sentence starters that are capitalised for being first, not for being a
/// name — a title's first word is a subject candidate only when it is not
/// one of these.
const SUBJECT_STARTERS: [&str; 40] = [
    "it", "in", "the", "until", "every", "before", "after", "when", "if", "a", "an", "on", "at",
    "for", "to", "not", "no", "never", "always", "there", "this", "that", "these", "those", "we",
    "our", "one", "all", "any", "some", "use", "do", "under", "since", "while", "with", "without",
    "as", "by", "from",
];

/// Capitalised tokens that could name what a title is about.
fn subject_tokens(title: &str) -> std::collections::HashSet<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .enumerate()
        .filter(|(i, w)| {
            w.chars().next().is_some_and(char::is_uppercase)
                && !(*i == 0 && SUBJECT_STARTERS.contains(&w.to_lowercase().as_str()))
        })
        .map(|(_, w)| w.to_lowercase())
        .collect()
}

/// Do two titles speak about the same thing? NLI's co-reference guard: MNLI
/// models presuppose co-reference, so two facts about two things read as a
/// contradiction (RefNLI). Passes, returning the subject tokens, when:
/// - **named**: both titles carry capitalised names and share one;
/// - **unnamed**: at most one side has a name and the titles share a run of
///   two or more content words that no differing word qualifies ("hazel bayou
///   assay station" vs "wheat dale assay station" is refused).
///
/// `None` means no NLI nomination.
pub fn shared_subject(a: &str, b: &str) -> Option<std::collections::HashSet<String>> {
    // Names come from the first clause only: a name in the tail is a detail, not
    // what the title is about.
    let (sa, sb) = (
        subject_tokens(title_clause(a)),
        subject_tokens(title_clause(b)),
    );
    if !sa.is_empty() && !sb.is_empty() {
        return (!sa.is_disjoint(&sb)).then(|| sa.union(&sb).cloned().collect());
    }
    // The run is looked for in the first clauses too: a phrase two titles
    // share in their tails ("… — Release green with nine assets" / "…; all
    // nine assets") is a detail they have in common, not a subject.
    let (ta, tb) = (title_tokens(title_clause(a)), title_tokens(title_clause(b)));
    let is_content =
        |w: &str| w.len() >= 3 && w.chars().any(char::is_alphabetic) && !is_function_word(w);
    // Every maximal common run, earliest in `a` first: the subject phrase
    // leads a claim more often than it trails one.
    for i in 0..ta.len() {
        for j in 0..tb.len() {
            if ta[i] != tb[j] || (i > 0 && j > 0 && ta[i - 1] == tb[j - 1]) {
                continue; // not a run start, or not maximal
            }
            let len = ta[i..]
                .iter()
                .zip(&tb[j..])
                .take_while(|(x, y)| x == y)
                .count();
            if len < 2 || ta[i..i + len].iter().filter(|w| is_content(w)).count() < 2 {
                continue;
            }
            // A differing content word right before the run on either side
            // is a modifier that makes the subject a different one.
            let qualified = |t: &[String], k: usize| k > 0 && is_content(&t[k - 1]);
            if qualified(&ta, i) || qualified(&tb, j) {
                continue;
            }
            return Some(
                ta[i..i + len.min(SUBJECT_PHRASE_MAX)]
                    .iter()
                    .cloned()
                    .collect(),
            );
        }
    }
    None
}

/// Do two titles demonstrably name different things? The refusing half of
/// [`shared_subject`], for the duplicate match. `false` when undecidable, since
/// a retitled note with the same body is still a duplicate.
pub(crate) fn subjects_differ(a: &str, b: &str) -> bool {
    let (sa, sb) = (
        subject_tokens(title_clause(a)),
        subject_tokens(title_clause(b)),
    );
    if !sa.is_empty() && !sb.is_empty() {
        return sa.is_disjoint(&sb);
    }
    let (ta, tb) = (title_tokens(title_clause(a)), title_tokens(title_clause(b)));
    let is_content =
        |w: &str| w.len() >= 3 && w.chars().any(char::is_alphabetic) && !is_function_word(w);
    let mut qualified_run = false;
    for i in 0..ta.len() {
        for j in 0..tb.len() {
            if ta[i] != tb[j] || (i > 0 && j > 0 && ta[i - 1] == tb[j - 1]) {
                continue;
            }
            let len = ta[i..]
                .iter()
                .zip(&tb[j..])
                .take_while(|(x, y)| x == y)
                .count();
            if len < 2 || ta[i..i + len].iter().filter(|w| is_content(w)).count() < 2 {
                continue;
            }
            let qualified = |t: &[String], k: usize| k > 0 && is_content(&t[k - 1]);
            if qualified(&ta, i) || qualified(&tb, j) {
                qualified_run = true;
            } else {
                return false; // an unqualified shared run: the same subject
            }
        }
    }
    qualified_run
}

/// Longest subject phrase the unnamed guard keeps out of the overlap count:
/// four words covers "amber harbor lease broker"; past that a shared run is
/// the claim itself and its words must stay in the count.
const SUBJECT_PHRASE_MAX: usize = 4;

/// A title's first clause: what precedes an em dash or a semicolon. The
/// claim leads; what follows is commentary on it.
pub fn title_clause(t: &str) -> &str {
    let t = t.split(" — ").next().unwrap_or(t);
    t.split("; ").next().unwrap_or(t).trim()
}

/// Lowercase alphanumeric tokens of a title.
fn title_tokens(t: &str) -> Vec<String> {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn is_function_word(w: &str) -> bool {
    CONTENT_STOP.contains(&w) || SUBJECT_STARTERS.contains(&w)
}

/// May the NLI path read this pair of titles? Same subject
/// ([`shared_subject`]) and at least [`crate::policy::CONFLICT_NLI_OVERLAP`]
/// content words shared beyond it ([`content_overlap`]). Public so the
/// bench's gate probe measures the shipped rule rather than a copy of it.
pub fn title_pair_admissible(a: &str, b: &str) -> bool {
    title_pair_admission(a, b).is_some()
}

/// [`title_pair_admissible`] with the reason: `"named"` when both titles
/// carry a shared capitalised name, `"unnamed"` when the lowercase run
/// guard admitted them, `None` when refused. For probes and replays that
/// compare the two paths.
pub fn title_pair_admission(a: &str, b: &str) -> Option<&'static str> {
    let (sa, sb) = (
        subject_tokens(title_clause(a)),
        subject_tokens(title_clause(b)),
    );
    let named = !sa.is_empty() && !sb.is_empty();
    let subject = shared_subject(a, b)?;
    (content_overlap(a, b, &subject) >= crate::policy::CONFLICT_NLI_OVERLAP).then_some(if named {
        "named"
    } else {
        "unnamed"
    })
}

/// Function words that carry no claim, for the overlap count.
const CONTENT_STOP: [&str; 60] = [
    "the",
    "a",
    "an",
    "is",
    "are",
    "was",
    "were",
    "it",
    "its",
    "in",
    "on",
    "of",
    "for",
    "to",
    "and",
    "or",
    "that",
    "this",
    "what",
    "which",
    "who",
    "how",
    "did",
    "does",
    "do",
    "we",
    "our",
    "us",
    "you",
    "be",
    "been",
    "with",
    "from",
    "by",
    "at",
    "as",
    "if",
    "not",
    "no",
    "yes",
    "about",
    "into",
    "than",
    "then",
    "so",
    "up",
    "case",
    "every",
    "without",
    "exception",
    "any",
    "all",
    "one",
    "until",
    "after",
    "before",
    "has",
    "have",
    "had",
    "never",
];

/// Content words two titles share beyond their subject: up to three common
/// leading words are stripped, then words of 3+ characters (minus function and
/// subject words) compare on their first five characters.
pub(crate) fn content_overlap(
    a: &str,
    b: &str,
    subject: &std::collections::HashSet<String>,
) -> usize {
    let (ta, tb) = (title_tokens(a), title_tokens(b));
    // The subject phrase is the common prefix, capped at three words: past
    // that a shared prefix is the claim itself with a clause appended, and
    // the claim's words must stay in the count.
    let prefix = ta
        .iter()
        .zip(&tb)
        .take_while(|(x, y)| x == y)
        .count()
        .min(3);
    let words = |t: &[String]| -> std::collections::HashSet<String> {
        t.iter()
            .filter(|w| {
                w.len() >= 3 && !CONTENT_STOP.contains(&w.as_str()) && !subject.contains(*w)
            })
            .map(|w| w.chars().take(5).collect())
            .collect()
    };
    words(&ta[prefix..])
        .intersection(&words(&tb[prefix..]))
        .count()
}
