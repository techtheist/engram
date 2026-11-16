//! Finding, spawning and version-checking the one machine core.

use crate::*;

/// The thin-client resolution: a healthy daemon that owns this db
/// — daemon.json next to the store, /health answers, and the advertised db
/// matches. When one exists, IT holds the file; everything else talks HTTP.
pub(crate) fn daemon_for(db: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(db.parent()?.join("daemon.json")).ok()?;
    let port = serde_json::from_str::<serde_json::Value>(&raw).ok()?["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())?;
    let health = doctor::http_get(port, "/health")?;
    let canon = std::fs::canonicalize(db).unwrap_or_else(|_| db.to_path_buf());
    health
        .contains(&canon.display().to_string())
        .then_some(port)
}

/// Start the core detached (output to `~/.engram/core.log`) and wait up to
/// 180 s for health: a first run downloads models. `--fake-embeddings` and the
/// environment pass through.
pub(crate) fn spawn_core_and_wait(
    http_port: Option<u16>,
    fake_embeddings: bool,
) -> anyhow::Result<u16> {
    let exe = std::env::current_exe().context("locating this binary")?;
    let home = registry::engram_home().context("no home directory for the engram core")?;
    let _ = std::fs::create_dir_all(&home);
    let log_path = home.join("core.log");
    // A failed log create must not abort the spawn (the 0.8.5 lesson) — the
    // core just runs with its output discarded.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    let (stdout, stderr) = match log {
        Some(f) => match f.try_clone() {
            Ok(c) => (c.into(), f.into()),
            Err(_) => (std::process::Stdio::null(), f.into()),
        },
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("core");
    if let Some(port) = http_port {
        cmd.args(["--http-port", &port.to_string()]);
    }
    if fake_embeddings {
        cmd.arg("--fake-embeddings");
    }
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("spawning the engram core")?;
    for _ in 0..360 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(port) = machine_core() {
            return Ok(port);
        }
        // An immediately-dead child (unwritable home, broken install) must
        // not cost the full 180s poll. Exiting isn't always dying, though —
        // `core` exits 0 when another core won the startup race — so a core
        // that advertises healthy right after the exit still counts.
        if let Ok(Some(status)) = child.try_wait() {
            if let Some(port) = machine_core() {
                return Ok(port);
            }
            anyhow::bail!(
                "the engram core exited during startup ({status}) — see {}",
                log_path.display()
            )
        }
    }
    anyhow::bail!(
        "the engram core didn't come up within 180s — see {}",
        log_path.display()
    )
}

/// Single-flight ensure-core: one caller spawns, concurrent callers wait and
/// find it. Called after the stdio handshake, so a slow boot delays binding,
/// never initialize.
pub(crate) fn ensure_machine_core(fake_embeddings: bool) -> anyhow::Result<u16> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _flight = GATE.locked();
    // Version handshake: an older core is retired here so the spawn below
    // replaces it with this binary — otherwise a client relaunching its
    // bridges right after an update would keep converging on the old core
    // (or resurrect one from an old binary) forever.
    retire_older_core();
    if let Some(port) = machine_core() {
        return Ok(port);
    }
    tracing::info!("no engram core on this machine — starting one");
    spawn_core_and_wait(None, fake_embeddings)
}

/// A healthy machine core, if one runs: `~/.engram/daemon.json` (beside the
/// registry — new state folds into the existing file family) verified over
/// /health.
pub(crate) fn machine_core() -> Option<u16> {
    machine_core_info().map(|(port, _)| port)
}

/// The healthy machine core's port plus the version its /health advertises
/// (older cores may not advertise one).
pub(crate) fn machine_core_info() -> Option<(u16, Option<String>)> {
    let path = registry::engram_home()?.join("daemon.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let port = serde_json::from_str::<serde_json::Value>(&raw).ok()?["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())?;
    let health = doctor::http_get(port, "/health")?;
    let version = serde_json::from_str::<serde_json::Value>(&health)
        .ok()
        .and_then(|v| v["version"].as_str().map(str::to_string));
    Some((port, version))
}

/// "X.Y.Z" (any suffix ignored) for the newer-binary comparison.
pub(crate) fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Is the advertised core version strictly older than this binary? Unknown
/// or unparsable versions are never "older" — converge, don't fight.
pub(crate) fn core_is_older(advertised: Option<&str>) -> bool {
    version_precedes(advertised, &engram_core::advertised_version())
}

/// The pure comparison behind [`core_is_older`]: strictly older, and ONLY
/// when both sides parse — a version-silent or unparsable core (a dev build,
/// a corrupt /health) is converged on, never fought, and an unparsable SELF
/// never claims to be newer than anything.
pub(crate) fn version_precedes(theirs: Option<&str>, ours: &str) -> bool {
    let (Some(t), Some(o)) = (theirs.and_then(parse_semver), parse_semver(ours)) else {
        return false;
    };
    t < o
}

/// Version handshake (issue #8): a healthy core older than this binary is
/// stopped so the normal path starts a current one. Newer or version-silent
/// cores are left alone. Quiet on stdout (bridges use it). Returns whether it
/// took over.
pub(crate) fn retire_older_core() -> bool {
    let Some((port, version)) = machine_core_info() else {
        return false;
    };
    if !core_is_older(version.as_deref()) {
        return false;
    }
    tracing::info!(
        "machine core on port {port} is v{} — this binary is v{}; restarting it",
        version.as_deref().unwrap_or("unknown"),
        engram_core::advertised_version()
    );
    let Some(home) = registry::engram_home() else {
        return false;
    };
    stop_machine_core(&home.join("daemon.json"), true) > 0
}

/// The machine core's advertised pid, from the same file [`machine_core`]
/// health-verifies.
pub(crate) fn machine_core_pid() -> Option<u32> {
    let raw = std::fs::read_to_string(registry::engram_home()?.join("daemon.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw).ok()?["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
}

#[cfg(test)]
mod version_handshake_tests {
    use super::{parse_semver, version_precedes};

    #[test]
    pub(crate) fn parse_semver_accepts_release_shapes_and_ignores_suffixes() {
        assert_eq!(parse_semver("0.9.0"), Some((0, 9, 0)));
        assert_eq!(parse_semver("v0.9.0"), Some((0, 9, 0)));
        assert_eq!(parse_semver(" 10.20.30 "), Some((10, 20, 30)));
        // Pre-release / build suffixes never change the comparison key.
        assert_eq!(parse_semver("0.9.0-rc1"), Some((0, 9, 0)));
        assert_eq!(parse_semver("0.9.0+build5"), Some((0, 9, 0)));
        assert_eq!(parse_semver("1.2.3.4"), Some((1, 2, 3)));
    }

    #[test]
    pub(crate) fn parse_semver_refuses_what_it_cannot_read() {
        for v in ["", "dev", "0.9", "0", "a.b.c", "0.9.x", "-", "…"] {
            assert_eq!(parse_semver(v), None, "{v:?} must not parse");
        }
    }

    /// The unsuccessful-handshake matrix: every ambiguous shape converges
    /// (false) — the takeover fires ONLY on a provable strictly-older core.
    #[test]
    pub(crate) fn handshake_fires_only_on_a_provably_older_core() {
        // The one true positive.
        assert!(version_precedes(Some("0.8.13"), "0.9.0"));
        assert!(
            version_precedes(Some("0.9.9"), "0.10.0"),
            "numeric, not lexicographic"
        );
        // Equal or newer: converge / never downgrade.
        assert!(!version_precedes(Some("0.9.0"), "0.9.0"));
        assert!(!version_precedes(Some("99.0.0"), "0.9.0"));
        // A version-silent core (pre-0.8.13 /health) is never "older".
        assert!(!version_precedes(None, "0.9.0"));
        // An unparsable advertised version (dev build, corrupt health).
        assert!(!version_precedes(Some("dev"), "0.9.0"));
        assert!(!version_precedes(Some(""), "0.9.0"));
        assert!(!version_precedes(Some("0.9"), "0.9.0"));
        // An unparsable SELF never claims seniority over anyone.
        assert!(!version_precedes(Some("0.0.1"), "dev"));
        assert!(!version_precedes(Some("0.0.1"), ""));
        // Suffixes collapse to the same key — an rc never fights its release.
        assert!(!version_precedes(Some("0.9.0-rc1"), "0.9.0"));
    }
}
