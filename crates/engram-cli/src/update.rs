//! Update awareness — the shared `releases/latest` query behind
//! `engram-alpha update`, plus the two notification tiers built on it:
//! `doctor` always asks (running it is an explicit diagnostic gesture),
//! `serve` asks at most once per 24h, off the startup path, and only ever
//! says one line. Nothing here installs anything — that stays a deliberate
//! `engram-alpha update`. Same constraint as self-update: system curl, no
//! bundled HTTP client.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Context;

pub const REPO: &str = "techtheist/engram";

/// Once a day is enough for a notice.
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Tag of the newest published release, via system curl. Capped at 10s so a
/// stalled network can't hang doctor.
pub fn latest_release_tag() -> anyhow::Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()
        .context("running curl (is it installed?)")?;
    anyhow::ensure!(out.status.success(), "could not query the latest release");
    let body = String::from_utf8_lossy(&out.stdout);
    body.split("\"tag_name\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .map(str::to_string)
        .context("parsing the latest release tag")
}

/// `Some(tag)` when the newest published release is strictly ahead of this
/// binary; network and parse failures are `Err` for the caller to soften.
pub fn newer_release() -> anyhow::Result<Option<String>> {
    let tag = latest_release_tag()?;
    Ok(is_newer(&tag).then_some(tag))
}

/// Strictly ahead of the running binary? Tags this scheme doesn't understand
/// (pre-releases, oddities) never trigger a notice.
fn is_newer(tag: &str) -> bool {
    match (parse_version(tag), parse_version(env!("CARGO_PKG_VERSION"))) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// "v0.8.6" / "0.8.6" → a comparable triple; `None` for anything else.
fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse().ok();
    let triple = (next()?, next()?, next()?);
    parts.next().is_none().then_some(triple)
}

/// Fire-and-forget notice for `serve`: at most one GitHub query per 24h
/// across restarts (timestamped cache in `~/.engram/update-check.json`),
/// spawned off the startup path so the daemon never waits on the network,
/// and silent on any failure — offline is a supported condition. This is
/// the only background network call the daemon makes; `ENGRAM_UPDATE_CHECK=0`
/// turns it off.
pub fn notify_on_newer_release() {
    if std::env::var("ENGRAM_UPDATE_CHECK").as_deref() == Ok("0") {
        return;
    }
    let Some(path) = cache_path() else { return };
    std::thread::spawn(move || {
        let now = unix_now();
        let cached: Option<serde_json::Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        if let Some(c) = &cached {
            let checked_at = c["checked_at"].as_u64().unwrap_or(0);
            if now.saturating_sub(checked_at) < CHECK_INTERVAL_SECS {
                // Inside the window: no network — but a restart still gets
                // the notice an earlier check earned.
                announce(c["latest"].as_str().unwrap_or_default());
                return;
            }
        }
        let Ok(tag) = latest_release_tag() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = serde_json::json!({ "checked_at": now, "latest": tag });
        let _ = std::fs::write(&path, format!("{body:#}\n"));
        announce(&tag);
    });
}

/// One info line, only when strictly newer — the daemon never nags.
fn announce(tag: &str) {
    if is_newer(tag) {
        tracing::info!(
            "a newer release ({tag}) is available — run `engram-alpha update` (this is v{})",
            env!("CARGO_PKG_VERSION")
        );
    }
}

/// The serve-side throttle cache, beside the other machine-level state
/// (rides `ENGRAM_HOME` like everything else).
fn cache_path() -> Option<PathBuf> {
    engram_core::registry::engram_home().map(|home| home.join("update-check.json"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The shakedown before a swap (0.10.4, from the issue #17 report): run the
/// new binary with `--version` and refuse it unless it starts and names
/// itself. A binary built against a newer glibc fails right here with the
/// loader's own words ("GLIBC_2.43 not found") instead of after it replaced
/// the one that worked. Bounded by `limit` so a binary that hangs can't hang
/// the update. Returns the version line it printed.
pub fn smoke_test(bin: &Path, limit: Duration) -> anyhow::Result<String> {
    let mut child = std::process::Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("the new binary could not be started ({})", bin.display()))?;
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "the new binary did not answer --version within {}s",
                limit.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let read = |pipe: Option<&mut dyn std::io::Read>| {
        let mut text = String::new();
        if let Some(pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text.trim().to_string()
    };
    let stdout = read(child.stdout.as_mut().map(|p| p as &mut dyn std::io::Read));
    let stderr = read(child.stderr.as_mut().map(|p| p as &mut dyn std::io::Read));
    if !status.success() {
        let said = if stderr.is_empty() { &stdout } else { &stderr };
        anyhow::bail!(
            "the new binary does not start on this machine ({status}){}",
            if said.is_empty() {
                String::new()
            } else {
                format!(":\n{said}")
            }
        );
    }
    anyhow::ensure!(
        stdout.starts_with("engram-alpha "),
        "the new binary answered --version with {stdout:?}, not an engram-alpha version"
    );
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("v0.8.6"), Some((0, 8, 6)));
        assert_eq!(parse_version("0.8.6"), Some((0, 8, 6)));
        assert_eq!(parse_version("v1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("v0.9.0-rc1"), None);
        assert_eq!(parse_version("v0.9"), None);
        assert_eq!(parse_version("v0.9.0.1"), None);
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn newer_means_strictly_ahead_of_this_binary() {
        let current = parse_version(env!("CARGO_PKG_VERSION")).expect("workspace version parses");
        assert!(
            !is_newer(env!("CARGO_PKG_VERSION")),
            "same version is not newer"
        );
        assert!(
            is_newer(&format!("v{}.{}.{}", current.0, current.1, current.2 + 1)),
            "next patch is newer"
        );
        assert!(
            is_newer(&format!("v{}.0.0", current.0 + 1)),
            "next major is newer"
        );
        assert!(!is_newer("v0.0.1"), "ancient tag is not newer");
        assert!(!is_newer("v99.0.0-rc1"), "unparseable tag never notifies");
    }

    /// A stand-in binary: a shell script with the given body.
    #[cfg(unix)]
    fn fake_bin(name: &str, body: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("engram-smoke-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("engram-alpha");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, bin)
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_passes_a_binary_that_names_itself() {
        let (dir, bin) = fake_bin("ok", "echo 'engram-alpha 0.10.4'");
        let line = smoke_test(&bin, Duration::from_secs(5)).expect("a working binary passes");
        assert_eq!(line, "engram-alpha 0.10.4");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_refuses_a_binary_the_loader_rejects_and_quotes_it() {
        // What issue #17's binary printed on Ubuntu 24.04.
        let (dir, bin) = fake_bin(
            "glibc",
            "echo \"engram-alpha: /lib/x86_64-linux-gnu/libm.so.6: version \\`GLIBC_2.43' not found\" >&2; exit 1",
        );
        let err = smoke_test(&bin, Duration::from_secs(5))
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not start on this machine"), "{err}");
        assert!(
            err.contains("GLIBC_2.43"),
            "the loader's words reach the user: {err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_refuses_a_binary_that_is_not_engram() {
        let (dir, bin) = fake_bin("other", "echo 'something else 1.0'");
        let err = smoke_test(&bin, Duration::from_secs(5))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an engram-alpha version"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn smoke_test_gives_up_on_a_binary_that_hangs() {
        let (dir, bin) = fake_bin("hang", "exec sleep 30");
        let start = Instant::now();
        let err = smoke_test(&bin, Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not answer --version"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(10), "the limit holds");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn smoke_test_refuses_a_file_that_cannot_run() {
        let dir = std::env::temp_dir().join(format!("engram-smoke-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("engram-alpha.bin");
        std::fs::write(&bin, b"not a binary").unwrap();
        assert!(smoke_test(&bin, Duration::from_secs(5)).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
