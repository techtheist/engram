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
pub fn scrub(text: &str) -> String {
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

/// A long token is treated as a secret when it has high Shannon entropy and
/// mixes letters with digits — the signature of random credential material,
/// not prose or a dotted identifier.
fn looks_secret(tok: &str) -> bool {
    if tok == MASK.trim_matches(['[', ']']) {
        return false;
    }
    let has_alpha = tok.bytes().any(|b| b.is_ascii_alphabetic());
    let has_digit = tok.bytes().any(|b| b.is_ascii_digit());
    has_alpha && has_digit && shannon_bits_per_char(tok) >= 3.5
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
}
