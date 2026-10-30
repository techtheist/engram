//! install.sh's shakedown (0.10.4, from the issue #17 report): the installer
//! starts the downloaded binary once with `--version` before it replaces
//! anything, so a release that can't run here never overwrites one that can.
//! Each test serves a fake release from a file:// directory
//! (`ENGRAM_RELEASES_URL`) whose "binary" is a shell script.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VERSION: &str = "v9.9.9";

fn target() -> &'static str {
    if cfg!(target_os = "macos") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

struct Release {
    root: PathBuf,
}

impl Release {
    /// A release directory whose engram-alpha runs `body`.
    fn new(tag: &str, body: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("engram-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stage = root.join("stage");
        let assets = root.join("releases").join(VERSION);
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let bin = stage.join("engram-alpha");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let asset = format!("engram-alpha-{VERSION}-{}.tar.gz", target());
        let packed = Command::new("tar")
            .arg("-czf")
            .arg(assets.join(&asset))
            .arg("-C")
            .arg(&stage)
            .arg("engram-alpha")
            .status()
            .unwrap();
        assert!(packed.success());
        let summed = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "(sha256sum {asset} 2>/dev/null || shasum -a 256 {asset}) > {asset}.sha256"
            ))
            .current_dir(&assets)
            .status()
            .unwrap();
        assert!(summed.success());
        Self { root }
    }

    fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    fn install(&self) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh");
        Command::new("sh")
            .arg(script)
            .env("ENGRAM_VERSION", VERSION)
            .env("ENGRAM_BIN_DIR", self.bin_dir())
            .env(
                "ENGRAM_RELEASES_URL",
                format!("file://{}", self.root.join("releases").display()),
            )
            .env("HOME", &self.root)
            .output()
            .expect("running install.sh")
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn installer_refuses_a_binary_that_cannot_start_and_keeps_the_old_one() {
    // What issue #17's binary printed on Ubuntu 24.04.
    let release = Release::new(
        "broken",
        "echo \"engram-alpha: /lib/x86_64-linux-gnu/libm.so.6: version \\`GLIBC_2.43' not found\" >&2; exit 1",
    );
    let installed = release.bin_dir().join("engram-alpha");
    std::fs::write(&installed, "#!/bin/sh\necho 'engram-alpha 0.10.2'\n").unwrap();

    let out = release.install();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the install fails: {out:?}");
    assert!(
        stderr.contains("GLIBC_2.43"),
        "the loader's words reach the user: {stderr}"
    );
    assert!(stderr.contains("nothing was replaced"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        "#!/bin/sh\necho 'engram-alpha 0.10.2'\n",
        "the working binary is untouched"
    );
    assert!(
        !release.bin_dir().join(".engram-alpha.new").exists(),
        "the staged binary is cleaned up"
    );
}

#[test]
fn installer_refuses_a_binary_that_is_not_engram() {
    let release = Release::new("impostor", "echo 'something else 1.0'");
    let out = release.install();
    assert!(!out.status.success(), "the install fails: {out:?}");
    assert!(
        !release.bin_dir().join("engram-alpha").exists(),
        "nothing installed"
    );
    assert!(!release.bin_dir().join(".engram-alpha.new").exists());
}

#[test]
fn installer_installs_a_binary_that_passes_the_shakedown() {
    let release = Release::new("good", "echo 'engram-alpha 9.9.9'");
    let out = release.install();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the install succeeds: {out:?}");
    assert!(
        stdout.contains("shakedown: engram-alpha 9.9.9 starts on this machine"),
        "{stdout}"
    );
    let installed = release.bin_dir().join("engram-alpha");
    let ran = Command::new(&installed).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout).trim(),
        "engram-alpha 9.9.9"
    );
    assert!(!release.bin_dir().join(".engram-alpha.new").exists());
}
