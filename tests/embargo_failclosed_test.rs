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

/// A source repo with one commit, plus a project whose policy seals to
/// `key_id`. Returns (src, proj).
fn project(tmp: &Path, key_id: &str) -> (std::path::PathBuf, std::path::PathBuf) {
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

/// The bundle must stand alone. `replay` borrows the store's objects through
/// `alternates`, an absolute path that means nothing on a maintainer's
/// machine, so the seal has to fold the objects in and cut the link. If it
/// does not, the recipient gets a repo whose history is unreadable while
/// `embargo-verify` happily reports success.
#[test]
fn test_sealed_bundle_is_self_contained() {
    let tmp = scratch("selfcontained");
    let (gpg_home, key_id) = make_test_key();
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    // Several commits, so there is real history to lose. They must all be
    // committed BEFORE the import, or the store never sees them.
    git(&src, &["init", "--quiet", "-b", "main"]);
    for i in 0..4 {
        std::fs::write(src.join("README.md"), format!("v{i}\n")).unwrap();
        git(&src, &["add", "."]);
        git(&src, &["commit", "-m", &format!("change {i}")]);
    }
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
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(ok, "seal failed: {msg}");

    let received = tmp.join("received");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(ok, "verify failed: {msg}");

    let repo = received.join("bundle/repo");
    assert!(
        !repo.join(".git/objects/info/alternates").exists(),
        "a bundle that borrows the sender's odb is unreadable off-machine"
    );

    // Hide the store, then read the history: this is what a maintainer has.
    let hidden = tmp.join("hidden-store");
    std::fs::rename(proj.join(".oot/objects.git"), &hidden).unwrap();
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let log = git(&repo, &["log", "--oneline"]);
    let packed = git(&repo, &["count-objects", "-v"]);
    std::fs::rename(&hidden, proj.join(".oot/objects.git")).unwrap();

    assert!(!head.is_empty(), "the received repo must have a HEAD");
    assert_eq!(
        log.lines().count(),
        4,
        "every change must travel with the bundle: {log}"
    );
    assert!(
        log.contains("change 3") && log.contains("change 0"),
        "messages must survive the handoff: {log}"
    );
    assert!(
        !packed.contains("in-pack: 0"),
        "objects must be packed into the bundle, not borrowed: {packed}"
    );

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The member check read the last field of tar's listing, which for a symlink
/// is its TARGET. A repo with an ordinary `-> ../shared` link was therefore
/// refused as an "unsafe member" and could never be verified, while the real
/// name went unchecked.
#[test]
fn test_verify_opens_bundle_with_symlinks() {
    let tmp = scratch("symlinks");
    let (gpg_home, key_id) = make_test_key();
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    std::os::unix::fs::symlink("README.md", src.join("alias")).unwrap();
    std::os::unix::fs::symlink("../src/other", src.join("up-link")).unwrap();
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

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(ok, "seal failed: {msg}");

    let received = tmp.join("received");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(
        ok,
        "a repo with symlinks must verify, not be refused as unsafe: {msg}"
    );
    assert!(received.join("bundle/repo").exists());

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The documented handoff uses a bare relative `--out`, with no directory
/// part. That derives a staging path whose parent is the empty string, and
/// `tar -C ""` fails, so the command the README prints could never work.
#[test]
fn test_seal_accepts_bare_relative_out() {
    let tmp = scratch("bareout");
    let (gpg_home, key_id) = make_test_key();
    let (_src, proj) = project(&tmp, &key_id);

    let artifact = Path::new("embargo-2099-01-01.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(
        ok,
        "a bare relative --out must work, it is what the docs print: {msg}"
    );
    assert!(
        proj.join(artifact).exists(),
        "artifact must land beside --out"
    );
    assert!(
        !proj.join("embargo-2099-01-01").exists(),
        "the plaintext staging dir must be cleaned up"
    );
    assert!(
        !proj.join("embargo-2099-01-01.tar").exists(),
        "the plaintext tar must be cleaned up"
    );

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The decrypted tar holds the whole bundle in plaintext while gpg writes
/// it, and gpg's `--output` creates files 0666 & ~umask. Oot has to make the
/// file itself at 0600, or the plaintext sits world-readable in whatever
/// directory the caller chose for the whole duration of the decrypt.
#[test]
#[cfg(unix)]
fn test_verify_temp_tar_is_private_while_gpg_writes_it() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};

    let tmp = scratch("perms");
    let (gpg_home, key_id) = make_test_key();
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    // Enough content that the decrypt takes long enough to sample.
    for i in 0..40 {
        std::fs::write(src.join(format!("f{i}.txt")), format!("payload {i}\n")).unwrap();
    }
    git(&src, &["init", "--quiet", "-b", "main"]);
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

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(ok, "seal failed: {msg}");

    let tar = tmp.join("received.decrypting.tar");
    let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let watcher = std::thread::spawn(move || {
        for _ in 0..100_000 {
            if let Ok(md) = std::fs::symlink_metadata(&tar) {
                let mode = md.permissions().mode() & 0o777;
                let mut seen = sink.lock().unwrap();
                if !seen.contains(&mode) {
                    seen.push(mode);
                }
            }
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    });

    let received = tmp.join("received");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(ok, "verify failed: {msg}");
    watcher.join().unwrap();

    let modes = seen.lock().unwrap().clone();
    assert!(
        !modes.is_empty(),
        "test setup: the temp tar was never observed, so this proves nothing"
    );
    for mode in &modes {
        assert_eq!(
            *mode & 0o077,
            0,
            "decrypted plaintext was group/world accessible (mode {mode:o})"
        );
    }

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// An unusable MANIFEST is a refusal, and the decrypted tree under `--out` is
/// plaintext: it must not survive the error, or the next run fails on
/// "output already exists" instead of the real problem.
#[test]
fn test_verify_clears_tree_when_manifest_is_unusable() {
    for case in ["missing", "corrupt"] {
        let tmp = scratch(&format!("manifest-{case}"));
        let (gpg_home, key_id) = make_test_key();
        let (_src, proj) = project(&tmp, &key_id);
        let gpg_home_str = gpg_home.to_str().unwrap().to_string();

        let stage = tmp.join("stage");
        std::fs::create_dir_all(stage.join("bundle")).unwrap();
        std::fs::write(stage.join("bundle/secret.txt"), "PLAINTEXT").unwrap();
        if case == "corrupt" {
            std::fs::write(stage.join("bundle/MANIFEST.json"), "not json at all").unwrap();
        }
        let tar_path = tmp.join("payload.tar");
        let made = Command::new("tar")
            .args(["-cf"])
            .arg(&tar_path)
            .arg("-C")
            .arg(&stage)
            .arg("--")
            .arg("bundle")
            .status()
            .expect("tar should run");
        assert!(made.success(), "tar create failed");

        let artifact = tmp.join("payload.tar.gpg");
        let sealed = Command::new("gpg")
            .env("GNUPGHOME", &gpg_home_str)
            .args([
                "--batch",
                "--yes",
                "--trust-model",
                "always",
                "--encrypt",
                "--sign",
                "--local-user",
                &key_id,
                "--recipient",
                &key_id,
                "--output",
                artifact.to_str().unwrap(),
                "--",
                tar_path.to_str().unwrap(),
            ])
            .status()
            .expect("gpg should run");
        assert!(sealed.success(), "gpg seal failed");

        let received = tmp.join("received");
        let (ok, msg) = oot_with_env(
            &[
                "embargo-verify",
                "--artifact",
                artifact.to_str().unwrap(),
                "--out",
                received.to_str().unwrap(),
            ],
            &proj,
            &[("GNUPGHOME", &gpg_home_str)],
        );
        assert!(!ok, "{case} manifest must be refused");
        assert!(
            !received.exists(),
            "{case}: a refused open must not leave decrypted plaintext: {msg}"
        );

        drop_key(&gpg_home);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
