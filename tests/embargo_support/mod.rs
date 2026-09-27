//! Shared scaffolding for the embargo integration tests.
//!
//! Every suite needs the same handful of things: run the built binary, run
//! git, make and destroy a throwaway keyring, and stand up a store that can
//! seal. Duplicating that per file meant a fix to the keyring teardown had to
//! be made three times, and two of the copies had already drifted.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_oot")
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Kriday")
        .env("GIT_AUTHOR_EMAIL", "k@oot.dev")
        .env("GIT_COMMITTER_NAME", "Kriday")
        .env("GIT_COMMITTER_EMAIL", "k@oot.dev")
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn oot(args: &[&str], cwd: &Path) -> (bool, String) {
    let o = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("oot binary should run");
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

pub fn oot_with_env(args: &[&str], cwd: &Path, extra: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(cwd);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let o = cmd.output().expect("oot binary should run");
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

pub fn make_test_key() -> (std::path::PathBuf, String) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir()
        .join(format!("oot-symlink-gpg-{}", std::process::id()))
        .join(format!("key-{seq}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let gen = Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "oot-symlink@example.com",
            "ed25519",
            "sign",
            "0",
        ])
        .output()
        .expect("gpg should run");
    assert!(gen.status.success(), "keygen failed");
    let list = Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args(["--list-secret-keys", "--with-colons"])
        .output()
        .expect("gpg should run");
    let fpr = String::from_utf8_lossy(&list.stdout)
        .lines()
        .find(|l| l.starts_with("fpr:"))
        .and_then(|l| l.split(':').nth(9))
        .expect("fingerprint")
        .to_string();
    let add = Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-add-key",
            &fpr,
            "cv25519",
            "encrypt",
            "0",
        ])
        .output()
        .expect("gpg should run");
    assert!(add.status.success(), "subkey add failed");
    (home, fpr)
}

pub fn drop_key(home: &Path) {
    let _ = Command::new("gpgconf")
        .env("GNUPGHOME", home)
        .args(["--kill", "gpg-agent"])
        .status();
    let _ = std::fs::remove_dir_all(home);
    // The parent is shared by every test in the run, so it can only be
    // removed once it is empty — `remove_dir`, never `remove_dir_all`, or a
    // sibling test's live keyring disappears mid-run. The last test out
    // tidies it, which is what stops the empty dirs piling up in /tmp.
    if let Some(parent) = home.parent() {
        if parent
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("oot-"))
        {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// A source repo with one commit, plus a project whose policy seals to
/// `key_id`. Returns (src, proj).
/// The name the oldest suite used for this.
#[allow(dead_code)]
pub fn drop_test_key(home: &Path) {
    drop_key(home)
}

pub fn project(tmp: &Path, key_id: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);
    std::fs::write(
        proj.join("visibility.toml"),
        format!(
            "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"{key_id}\"]\nresign_key_id = \"{key_id}\"\n"
        ),
    )
    .unwrap();
    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    (src, proj)
}

/// The fingerprint of a key identified by its user id.
pub fn gpg_fpr(home: &str, uid: &str) -> String {
    let out = Command::new("gpg")
        .env("GNUPGHOME", home)
        .args(["--list-keys", "--with-colons", uid])
        .output()
        .expect("gpg should run");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.starts_with("fpr:"))
        .and_then(|l| l.split(':').nth(9))
        .expect("fingerprint for {uid}")
        .to_string()
}

/// Raw colon output for a key, so a test can assert on its validity field.
pub fn gpg_fpr_state(home: &str, fpr: &str) -> (bool, String) {
    let out = Command::new("gpg")
        .env("GNUPGHOME", home)
        .args(["--list-keys", "--with-colons", fpr])
        .output()
        .expect("gpg should run");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

pub fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("oot-argcase-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
