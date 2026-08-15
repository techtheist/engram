//! Defense-in-depth secret scrubbing applied to every node `title`/`body`
//! before it is written (PLAN §6B). The skill is the first line of defense;
//! this is the backstop so a leaked credential never lands in the graph.
//!
//! Two layers: (1) named patterns for well-known secret shapes, and (2) a
//! high-entropy fallback that catches opaque tokens with no recognizable
//! prefix. False positives are biased toward over-redaction — losing a commit
//! SHA is cheaper than persisting a key.

use std::sync::LazyLock;

use regex::Regex;

const MASK: &str = "[REDACTED]";

/// Whole-match patterns: the entire match is replaced with the mask.
static WHOLE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // PEM private key blocks
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        // AWS access key id
        r"AKIA[0-9A-Z]{16}",
        // JSON Web Token
        r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
        // GitHub tokens
        r"gh[pousr]_[A-Za-z0-9]{20,}",
        // Slack tokens
        r"xox[baprs]-[A-Za-z0-9-]{10,}",
        // OpenAI-style keys
        r"sk-[A-Za-z0-9_-]{20,}",
        // credentials embedded in a URL (scheme://user:pass@host)
        r"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s:@/]+:[^\s:@/]+@",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static redaction pattern"))
    .collect()
});

/// `key = value` / `key: value` assignments — the value is masked, the key kept
/// so the note still reads sensibly.
static KV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(\b(?:api[_-]?key|secret[a-z_]*|access[_-]?token|token|password|passwd|pwd|bearer|authorization)\b\s*[:=]\s*)["']?[^\s"',;]{5,}"#,
    )
    .expect("static kv pattern")
});

/// Candidate opaque tokens for the entropy fallback.
static CANDIDATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/=_-]{24,}").expect("static candidate pattern"));

/// Scrub a single field. Idempotent: running it twice yields the same output.
///
/// Sealed history blobs pass untouched: they're base64 ciphertext, which the
/// entropy fallback would otherwise eat as a "secret" — and their plaintext
/// was scrubbed BEFORE sealing (Engine::add_history_node), so there is
/// nothing left here to find.
pub fn scrub(text: &str) -> String {
    if text.starts_with(crate::history::SEAL_PREFIX) {
        return text.to_string();
    }
    let mut out = text.to_string();
    for re in WHOLE.iter() {
        out = re.replace_all(&out, MASK).into_owned();
    }
    out = KV.replace_all(&out, format!("${{1}}{MASK}")).into_owned();
    out = CANDIDATE
        .replace_all(&out, |c: &regex::Captures| {
            let tok = &c[0];
            if looks_secret(tok) {
                MASK.to_string()
            } else {
                tok.to_string()
            }
        })
        .into_owned();
    out
}

/// A segment shorter than this carries no usable entropy signal: Shannon
/// bits/char is bounded by log2(len), so an 8-character segment cannot reach
/// the threshold even when every character differs. Short segments therefore
/// abstain rather than vote — they are exactly the `v3`/`en`/`gnu` fragments
/// that compound identifiers are made of.
const MIN_SEGMENT: usize = 12;

/// Is this token credential material?
///
/// STRUCTURE IS THE TELL (0.8.7, fixing 00apzse1dkm5). Entropy used to be
/// measured over the WHOLE token, which meant hyphen-joined dictionary words
/// looked random at the character level: `nli-deberta-v3-small` with an org
/// prefix, `x86_64-unknown-linux-gnu`, a reranker slug, any reddit permalink —
/// all masked, permanently, in the one system that exists to remember them.
/// Fifteen nodes lost facts that way before anyone noticed.
///
/// So entropy is now judged PER SEPARATOR-DELIMITED SEGMENT. Real credential
/// material has no dictionary-shaped parts: either it carries no separators at
/// all (one segment, and the old whole-token judgment applies unchanged), or
/// its segments are themselves random. A compound identifier fails that test
/// on every segment, which is the point.
///
/// This relaxes only the unnamed-token backstop. Every NAMED pattern above —
/// PEM, AWS, JWT, GitHub, Slack, OpenAI-style, `key = value` — is untouched,
/// and those are what actually catch secrets in practice.
fn looks_secret(tok: &str) -> bool {
    if tok == MASK.trim_matches(['[', ']']) {
        return false;
    }
    let segments: Vec<&str> = tok
        .split(['-', '_', '/', '+', '='])
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() > 1 {
        // Structured token: only segments long enough to carry a signal vote,
        // and the token is a secret only if every one of them looks random. A
        // mostly-random blob with a short suffix (`<32 random chars>-v2`) is
        // still caught, because its one long segment votes alone.
        let mut voted = false;
        for seg in segments.iter().filter(|s| s.len() >= MIN_SEGMENT) {
            if !segment_is_random(seg) {
                return false;
            }
            voted = true;
        }
        return voted;
    }
    segment_is_random(tok)
}

/// The original signature, now applied to one segment: high Shannon entropy
/// AND a letters/digits mix — random credential material, not prose.
fn segment_is_random(seg: &str) -> bool {
    let has_alpha = seg.bytes().any(|b| b.is_ascii_alphabetic());
    let has_digit = seg.bytes().any(|b| b.is_ascii_digit());
    has_alpha && has_digit && shannon_bits_per_char(seg) >= 3.5
}

fn shannon_bits_per_char(s: &str) -> f64 {
    let bytes = s.as_bytes();
    let len = bytes.len() as f64;
    let mut freq = [0u32; 256];
    for &b in bytes {
        freq[b as usize] += 1;
    }
    -freq
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            p * p.log2()
        })
        .sum::<f64>()
}

#[cfg(test)]
mod tests {
    use super::scrub;

    #[test]
    fn redacts_aws_key() {
        let out = scrub("creds AKIAIOSFODNN7EXAMPLE here");
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("AKIA"));
        assert!(out.contains("creds") && out.contains("here"));
    }

    #[test]
    fn redacts_key_value_but_keeps_key() {
        let out = scrub("set password: hunter2-very-secret-value");
        assert!(out.starts_with("set password:"));
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn redacts_jwt_and_github_and_pem() {
        assert!(
            scrub("token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc123def456").contains("[REDACTED]")
        );
        assert!(scrub("ghp_0123456789abcdefABCDEF0123456789xyz").contains("[REDACTED]"));
        let pem =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAA\nzzz\n-----END RSA PRIVATE KEY-----";
        let out = scrub(pem);
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("MIIBOgIBAAA"));
    }

    #[test]
    fn redacts_url_credentials() {
        let out = scrub("clone https://user:s3cr3tpass@github.com/x/y.git");
        assert!(!out.contains("s3cr3tpass"));
        assert!(out.contains("github.com"));
    }

    #[test]
    fn high_entropy_token_caught_without_keyword() {
        let out = scrub("value xQ7zP2mK9wL4vR8nT1cY6bF3hJ5dG0aS here");
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn keeps_prose_and_short_tokens() {
        let prose = "We chose SQLite WAL mode because concurrent reads matter for the pane.";
        assert_eq!(scrub(prose), prose);
        // a lowercase non-random long word should survive (no digits)
        let s = "internationalization handling";
        assert_eq!(scrub(s), s);
    }

    #[test]
    fn is_idempotent() {
        let once = scrub("password=abc123secretvalue99 AKIAIOSFODNN7EXAMPLE");
        assert_eq!(scrub(&once), once);
    }

    /// The regression for 00apzse1dkm5, written from the exact identifiers the
    /// old whole-token entropy rule ate. Every string here is one this graph
    /// lost at least once, and the Problem node had to describe them in pieces
    /// because it could not store them intact.
    #[test]
    fn compound_identifiers_survive_the_entropy_backstop() {
        for kept in [
            "cross-encoder/nli-deberta-v3-small",
            "sentence-transformers/all-MiniLM-L6-v2",
            "jinaai/jina-reranker-v2-base-multilingual",
            "MoritzLaurer/deberta-v3-base-zeroshot-v1.1-all-33",
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "tasksource/deberta-small-long-nli",
            "https://www.reddit.com/r/LocalLLaMA/comments/1n2x3y4/engram_local_memory/",
            "crates/engram-core/src/store_sqlite.rs",
            "engram-0.8.6-261-signed.zip",
        ] {
            assert_eq!(
                scrub(kept),
                kept,
                "the graph must be able to record {kept:?}"
            );
        }
    }

    /// The other half of the same change: relaxing the backstop must not open
    /// it. Structured tokens whose long segments ARE random still die.
    #[test]
    fn structure_does_not_excuse_randomness() {
        for masked in [
            // A random blob wearing a version suffix — one long segment votes
            // alone and convicts it.
            "xQ7zP2mK9wL4vR8nT1cY6bF3hJ5dG0aS-v2",
            // URL-safe base64 with separators between random runs.
            "dGhpc0lzQVNlY3JldFZhbHVl-M3hLOXBRc1o0dEcyd1k1-bkYxaEo3",
            // No separators at all: unchanged whole-token judgment.
            "xQ7zP2mK9wL4vR8nT1cY6bF3hJ5dG0aS",
        ] {
            let out = scrub(masked);
            assert!(out.contains("[REDACTED]"), "{masked:?} must not survive");
        }

        // And the named patterns are untouched by the relaxation — they are
        // what actually catches secrets, and several of them are hyphen- or
        // underscore-structured by design.
        assert!(scrub("ghp_0123456789abcdefABCDEF0123456789xyz").contains("[REDACTED]"));
        assert!(scrub("xoxb-1234567890-abcdefghijklmnop").contains("[REDACTED]"));
        assert!(scrub("sk-abcdefghijklmnopqrstuvwxyz0123456789").contains("[REDACTED]"));
        assert!(scrub("AKIAIOSFODNN7EXAMPLE").contains("[REDACTED]"));
    }
}
