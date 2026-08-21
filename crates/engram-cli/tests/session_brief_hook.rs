//! The SessionStart brief hook, exercised as the harness runs it.
//!
//! Three failure modes it must never repeat (all field-observed on 0.8.8, all
//! silent — an empty or foreign injection is indistinguishable from an empty
//! graph): asking the home-rooted machine core for the UNSCOPED brief, taking
//! an older daemon's answer to a `?project=` it ignores, and giving up when
//! `engram-alpha` is not on the hook's login-less PATH.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

const COLD_START: &str = "# Engram brief\n\nThe graph is empty — this is a cold start.\n";

fn hook_path() -> String {
    format!(
        "{}/../../hooks/session-brief.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// An engram-wired repo plus a private HOME, so the hook's daemon discovery
/// and binary probe both stay inside the sandbox.
fn sandbox(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let tmp = std::env::temp_dir().join(format!("engram-brief-hook-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    let home = tmp.join("home");
    fs::create_dir_all(repo.join(".engram")).unwrap();
    fs::create_dir_all(home.join(".engram")).unwrap();
    // Either backend counts as wired; tepin-born repos never have a graph.db.
    fs::write(repo.join(".engram/graph.tepin"), "").unwrap();
    (tmp, repo, home)
}

fn run_hook(repo: &Path, home: &Path) -> String {
    let out = Command::new("sh")
        .arg(hook_path())
        .env_clear()
        // Deliberately login-less: neither ~/.cargo/bin nor ~/.local/bin.
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("ENGRAM_HOME", home.join(".engram"))
        .env("CLAUDE_PROJECT_DIR", repo)
        .output()
        .expect("running the hook");
    assert!(
        out.status.success(),
        "a memory hook must never fail a session"
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A stand-in machine core: rooted at the home graph, so its unscoped brief
/// is home's cold-start text. `understands_project` distinguishes a current
/// core (resolves `?project=`, refuses one that names nothing) from an older
/// one (ignores the parameter and answers with its own graph anyway).
fn fake_core(repo: &Path, home: &Path, understands_project: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home_db = home.join(".engram/home.tepin").display().to_string();
    // The hook resolves symlinks (macOS /var → /private/var) and curl
    // percent-encodes the path separators — in lowercase, so compare folded.
    let root = fs::canonicalize(repo)
        .unwrap()
        .display()
        .to_string()
        .replace('/', "%2f")
        .to_lowercase();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let (status, body) = if path.starts_with("/health") {
                (200, format!("{{\"db\":\"{home_db}\",\"status\":\"ok\"}}"))
            } else if !path.starts_with("/brief") {
                (404, String::new())
            } else if !understands_project {
                (200, COLD_START.to_string())
            } else if path.to_lowercase().contains(&root) {
                (200, "# Engram brief\nSCOPED BRIEF\n".to_string())
            } else if path.contains("project=") {
                (400, "{\"error\":\"unknown project\"}".to_string())
            } else {
                (200, COLD_START.to_string())
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

fn write_daemon_files(dirs: &[std::path::PathBuf], port: u16) {
    for dir in dirs {
        fs::write(
            dir.join("daemon.json"),
            format!("{{\n  \"port\": {port},\n  \"pid\": 1\n}}\n"),
        )
        .unwrap();
    }
}

/// A stub `engram-alpha` reachable only through the hook's explicit probe of
/// the install dirs — never through PATH.
fn stub_binary(home: &Path) {
    let bin_dir = home.join(".cargo/bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let stub = bin_dir.join("engram-alpha");
    fs::write(
        &stub,
        "#!/bin/sh\necho '# Engram brief'\necho 'STUB BRIEF'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn brief_hook_asks_a_home_rooted_core_for_this_project() {
    let (tmp, repo, home) = sandbox("scoped");
    let port = fake_core(&repo, &home, true);
    write_daemon_files(&[repo.join(".engram"), home.join(".engram")], port);

    let text = run_hook(&repo, &home);
    assert!(
        text.contains("SCOPED BRIEF"),
        "the hook must name its project directory and brief THAT graph: {text:?}"
    );
    assert!(
        !text.contains("cold start"),
        "asking a home-rooted core for the unscoped brief is the bug: {text:?}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn brief_hook_never_trusts_a_daemon_that_ignores_the_project() {
    let (tmp, repo, home) = sandbox("legacy");
    let port = fake_core(&repo, &home, false);
    write_daemon_files(&[repo.join(".engram"), home.join(".engram")], port);
    stub_binary(&home);

    let text = run_hook(&repo, &home);
    assert!(
        text.contains("STUB BRIEF") && !text.contains("cold start"),
        "an older daemon answers `?project=` with its OWN graph; the hook must \
         fall through to the CLI instead of injecting it: {text:?}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn brief_hook_finds_the_binary_off_path() {
    let (tmp, repo, home) = sandbox("offpath");
    // No daemon anywhere: the CLI fallback is the only source left, and the
    // hook's login-less PATH cannot see it.
    stub_binary(&home);

    let text = run_hook(&repo, &home);
    assert!(
        text.contains("STUB BRIEF"),
        "the hook fell silent instead of probing the install dirs: {text:?}"
    );
    let _ = fs::remove_dir_all(&tmp);
}
