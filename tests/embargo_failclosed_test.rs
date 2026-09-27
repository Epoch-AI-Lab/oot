//! Regression tests for the seal error paths that must not leave a
//! shippable artifact behind.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_oot")
}

fn git(repo: &Path, args: &[&str]) -> String {
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

fn oot(args: &[&str], cwd: &Path) -> (bool, String) {
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

fn oot_with_env(args: &[&str], cwd: &Path, extra: &[(&str, &str)]) -> (bool, String) {
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

fn make_test_key() -> (std::path::PathBuf, String) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir()
        .join(format!("oot-failclosed-gpg-{}", std::process::id()))
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
            "oot-failclosed@example.com",
            "ed25519",
            "sign",
            "0",
        ])
        .output()
        .expect("gpg should run");
    assert!(
        gen.status.success(),
        "keygen failed: {}",
        String::from_utf8_lossy(&gen.stderr)
    );
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

fn drop_key(home: &Path) {
    let _ = Command::new("gpgconf")
        .env("GNUPGHOME", home)
        .args(["--kill", "gpg-agent"])
        .status();
    let _ = std::fs::remove_dir_all(home);
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("oot-failclosed-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A signer that passes the pre-flight (a public key exists for it) but
/// cannot actually sign — a key with no secret half — must fail the seal
/// and leave no partial artifact to ship. The plaintext is built first, so
/// this is the path where a partial `.tar.gpg` used to survive.
#[test]
fn test_seal_failure_leaves_no_partial_artifact() {
    let tmp = scratch("partial");
    let (gpg_home, key_id) = make_test_key();
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    // Import the recipient's PUBLIC key only, so the signer entry resolves
    // for recipients but has no secret key for signing.
    let sealed_home = tmp.join("sealed-home");
    std::fs::create_dir_all(&sealed_home).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sealed_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let pubkey = tmp.join("recipient.pub");
    let exported = Command::new("gpg")
        .env("GNUPGHOME", &gpg_home)
        .args(["--output", pubkey.to_str().unwrap(), "--export", &key_id])
        .status()
        .expect("gpg should run");
    assert!(exported.success(), "export failed");
    let imported = Command::new("gpg")
        .env("GNUPGHOME", &sealed_home)
        .args(["--import", pubkey.to_str().unwrap()])
        .status()
        .expect("gpg should run");
    assert!(imported.success(), "import failed");

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

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", sealed_home.to_str().unwrap())],
    );
    assert!(!ok, "seal without a signing secret key must fail");
    assert!(
        !artifact.exists(),
        "a failed seal must leave no partial artifact: {msg}"
    );
    assert!(
        !tmp.join("bundle").exists() && !tmp.join("bundle.tar").exists(),
        "a failed seal must clean its plaintext: {msg}"
    );
    // The refusal is audited, so the attempt is not invisible.
    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("seal-refused") || log.contains("seal-failed"),
        "the refusal must be audited: {log}"
    );

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A read-only parent makes the seal fail after the plaintext staging dir
/// exists. The command must not report success and must leave nothing.
#[test]
fn test_seal_into_readonly_parent_fails_cleanly() {
    let tmp = scratch("ro");
    let (gpg_home, key_id) = make_test_key();
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

    let ro = tmp.join("readonly");
    std::fs::create_dir_all(&ro).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let artifact = ro.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(!ok, "seal into a read-only parent must fail");
    assert!(!msg.contains("sealed"), "must not claim sealed: {msg}");
    assert_eq!(
        std::fs::read_dir(&ro).unwrap().count(),
        0,
        "no artifact or plaintext may survive"
    );

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}
