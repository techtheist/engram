//! `migrate`, `setup` and `update`.

use crate::*;

/// The §7C step-5 cutover, per repo: JSON export/import is the vehicle for
/// nodes + edges (embeddings regenerated on the way in), suspects and the
/// audit journal ride over verbatim, counts are verified, and the registry
/// entry repoints — the SQLite source is never modified.
pub(crate) fn run_migrate(args: MigrateArgs) -> anyhow::Result<()> {
    let src_path = args.db;
    anyhow::ensure!(
        src_path.exists(),
        "{} does not exist — nothing to migrate",
        src_path.display()
    );
    anyhow::ensure!(
        !engram_core::is_tepin_path(&src_path),
        "{} is already a TepinDB store",
        src_path.display()
    );
    let dst_path = src_path.with_extension("tepin");
    if dst_path.exists() {
        anyhow::ensure!(
            args.force,
            "{} already exists — pass --force to rebuild it from the SQLite source",
            dst_path.display()
        );
        std::fs::remove_file(&dst_path)
            .with_context(|| format!("removing {}", dst_path.display()))?;
    }

    let models = load_models(args.fake_embeddings, false)?;
    let summary = engram_core::migrate_to_tepin(
        &src_path,
        models.current().embedder,
        engram_core::AuditOrigin::cli(),
    )
    .with_context(|| format!("migrating {}", src_path.display()))?;

    // Repoint this repo's registry entry so every hub opens the new store.
    if let Some(root) = dst_path
        .canonicalize()
        .ok()
        .and_then(|p| {
            let dir = p.parent()?;
            (dir.file_name()? == ".engram").then(|| dir.parent().map(Path::to_path_buf))?
        })
        .or_else(|| std::env::current_dir().ok())
        && let Err(e) = registry::register(&root, &dst_path)
    {
        eprintln!("note: couldn't repoint ~/.engram/registry.json: {e}");
    }

    println!(
        "migrated {} nodes / {} edges / {} suspects / {} audit rows to {}",
        summary.nodes,
        summary.edges,
        summary.suspects,
        summary.audit,
        dst_path.display()
    );
    println!(
        "{} is untouched (your backup); every engram-alpha command now picks {} automatically — restart the daemon and reconnect MCP",
        src_path.display(),
        dst_path.file_name().unwrap_or_default().display()
    );
    Ok(())
}

pub(crate) fn run_setup(args: SetupArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    if args.refresh {
        let found = skillsync::refresh(&cwd, skillsync::Mode::All);
        let lines = skillsync::report(&cwd, &found);
        if lines.is_empty() {
            println!("installed skills and instruction blocks are current");
        }
        for line in lines {
            println!("{line}");
        }
        upkeep::record_setup(&cwd, &[], None, &found);
        return Ok(());
    }
    // A repo set up before remembers its assistants (issue #18): re-running
    // `setup` rewires those, not whatever happens to be installed.
    let record = engram_core::setup_record::load(&cwd);
    let skill = args
        .skill
        .clone()
        .or_else(|| record.as_ref().and_then(|r| r.variant.clone()))
        .or_else(|| skillsync::installed_variant(&cwd))
        .unwrap_or_else(|| "relaxed".into());
    let recorded: Vec<&str> = record
        .as_ref()
        .map(|r| {
            setup::AGENTS
                .iter()
                .copied()
                .filter(|a| r.agents.iter().any(|x| x == a))
                .collect()
        })
        .unwrap_or_default();
    let agents: Vec<&str> = match args.cli.as_deref() {
        Some("all") => setup::AGENTS.to_vec(),
        Some(list) => {
            let mut v = Vec::new();
            for a in list.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                let known = setup::AGENTS.iter().find(|k| **k == a).with_context(|| {
                    format!(
                        "unknown --cli '{a}' (claude|codex|gemini|opencode|kilo|antigravity|bob|windsurf|devin|cursor|all)"
                    )
                })?;
                v.push(*known);
            }
            v
        }
        None if !recorded.is_empty() => {
            println!("==> set up before for: {}", recorded.join(", "));
            recorded
        }
        None => {
            let detected = setup::detect_agents();
            anyhow::ensure!(
                !detected.is_empty(),
                "no supported assistants detected — pick explicitly with --cli"
            );
            println!("==> detected: {}", detected.join(", "));
            detected
        }
    };
    anyhow::ensure!(!agents.is_empty(), "nothing to wire");
    setup::Setup::new(&skill, args.mcp_only)?.run(&agents)?;
    // Wiring a repo makes it a project — put it on the machine registry so
    // every other project's hub can see it. Best-effort.
    if let Ok(cwd) = std::env::current_dir()
        && let Err(e) = registry::register(&cwd, &cwd.join(".engram/graph.db"))
    {
        eprintln!("note: couldn't add this repo to ~/.engram/registry.json: {e}");
    }
    Ok(())
}

/// Self-update from GitHub Releases: download the platform asset, verify its
/// published sha256, and atomically swap the running binary. Uses the system
/// curl/tar (present on macOS, Linux, WSL, and Windows 10+) so the binary
/// doesn't carry an HTTP client for one command.
pub(crate) fn run_update(args: UpdateArgs) -> anyhow::Result<()> {
    let (target, ext, bin_name) = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "tar.gz", "engram-alpha")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ("x86_64-unknown-linux-gnu", "tar.gz", "engram-alpha")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", "exe", "engram-alpha.exe")
    } else {
        anyhow::bail!("no prebuilt binaries for this platform — update from source instead");
    };

    let tag = match args.version {
        Some(v) => v,
        None => update::latest_release_tag()?,
    };

    let asset = format!("engram-alpha-{tag}-{target}.{ext}");
    let url = format!(
        "https://github.com/{}/releases/download/{tag}/{asset}",
        update::REPO
    );
    let tmp = std::env::temp_dir().join(format!("engram-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let cleanup = scopeguard(tmp.clone());
    let archive = tmp.join(&asset);

    eprintln!("downloading {asset}…");
    let fetch = |from: &str, to: &Path| -> anyhow::Result<()> {
        let status = std::process::Command::new("curl")
            .args(["-fL", "--progress-bar", "-o"])
            .arg(to)
            .arg(from)
            .status()
            .context("running curl")?;
        anyhow::ensure!(status.success(), "download failed: {from}");
        Ok(())
    };
    fetch(&url, &archive)?;
    let sums = tmp.join(format!("{asset}.sha256"));
    fetch(&format!("{url}.sha256"), &sums)?;

    let expected = std::fs::read_to_string(&sums)?
        .split_whitespace()
        .next()
        .map(str::to_lowercase)
        .context("empty checksum file")?;
    anyhow::ensure!(
        sha256_file(&archive)? == expected,
        "checksum mismatch — refusing to install"
    );

    let new_bin = if ext == "tar.gz" {
        let status = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&tmp)
            .status()
            .context("running tar")?;
        anyhow::ensure!(status.success(), "extracting {asset} failed");
        tmp.join(bin_name)
    } else {
        archive
    };

    let exe = std::env::current_exe().context("locating the running binary")?;
    if sha256_file(&new_bin)? == sha256_file(&exe)? {
        println!("already up to date ({tag})");
        drop(cleanup);
        return Ok(());
    }

    // Stage next to the target so the final rename is atomic (same filesystem).
    let staged = exe.with_extension("new");
    std::fs::copy(&new_bin, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    // Shakedown (issue #17): run the new binary with --version before the swap.
    // Unix tests the staged copy in place (a noexec temp dir can't fail it);
    // Windows needs the .exe name, so it tests the download.
    let candidate = if cfg!(windows) { &new_bin } else { &staged };
    match update::smoke_test(candidate, std::time::Duration::from_secs(10)) {
        Ok(line) => eprintln!("shakedown: {line} starts on this machine"),
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            drop(cleanup);
            anyhow::bail!(
                "{e:#}\nrefusing to install {tag}: {} is unchanged. To pin a release that works \
                 here, run `engram-alpha update --version <tag>`.",
                exe.display()
            );
        }
    }
    #[cfg(windows)]
    {
        // Windows can't replace a running exe, but it can rename it away.
        let old = exe.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old)?;
    }
    std::fs::rename(&staged, &exe)?;
    println!(
        "updated {} to {tag} — restart your daemon (engram-alpha serve)",
        exe.display()
    );
    for line in upkeep::report_from(&exe) {
        println!("{line}");
    }
    drop(cleanup);
    Ok(())
}

pub(crate) fn sha256_file(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Remove the temp dir when dropped, however the update exits.
pub(crate) fn scopeguard(dir: PathBuf) -> impl Drop {
    struct G(PathBuf);
    impl Drop for G {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    G(dir)
}
