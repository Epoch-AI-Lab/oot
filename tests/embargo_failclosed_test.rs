//! Regression tests for the seal error paths that must not leave a
//! shippable artifact behind.

use std::path::Path;
use std::process::Command;

#[path = "embargo_support/mod.rs"]
mod support;
use support::*;

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
    // --all, or a bundle that dropped a whole branch still looks complete.
    let log = git(&repo, &["log", "--oneline", "--all"]);
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
    // The links themselves must survive, and still point where they did.
    let repo = received.join("bundle/repo");
    assert!(repo.exists());
    assert_eq!(
        std::fs::read_link(repo.join("alias")).expect("alias must survive"),
        Path::new("README.md"),
        "alias target must be intact"
    );
    assert_eq!(
        std::fs::read_link(repo.join("up-link")).expect("up-link must survive"),
        Path::new("../src/other"),
        "up-link target must be intact"
    );

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
    // Poll until verify finishes rather than for a fixed count: a fixed
    // 100k iterations at 100us costs 10s whatever the decrypt takes.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&done);
    let watcher = std::thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
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
    done.store(true, std::sync::atomic::Ordering::Relaxed);
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

/// A signing key that expires AFTER it signed still stops the bundle
/// opening: gpg emits `EXPKEYSIG` beside `VALIDSIG`, and that marker refuses
/// the open. This was documented as the opposite for a while, on the strength
/// of a claim about gpg that nobody had run. It is a real operational
/// constraint — sign with a key that outlives the embargo, or expect to
/// re-seal — so it gets a test rather than a comment.
#[test]
fn test_verify_refuses_bundle_whose_signer_key_expired() {
    let tmp = scratch("expired-signer");
    let (gpg_home, key_id) = make_test_key();
    let gpg_home_str = gpg_home.to_str().unwrap().to_string();

    // A key with a two second life, and an encryption subkey that outlives
    // the seal so only the SIGNING key's expiry is what gets tested.
    let short = Command::new("gpg")
        .env("GNUPGHOME", &gpg_home_str)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "brief@example.com",
            "ed25519",
            "sign",
            "seconds=2",
        ])
        .status()
        .expect("gpg should run");
    assert!(short.success(), "short-lived keygen failed");
    let brief = gpg_fpr(&gpg_home_str, "brief@example.com");
    assert_ne!(brief, key_id, "test setup: a second key is required");
    // The sealing side needs an encryption subkey to encrypt TO, so give the
    // brief key one. Only the signing half is meant to expire.
    let enc = Command::new("gpg")
        .env("GNUPGHOME", &gpg_home_str)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-add-key",
            &brief,
            "cv25519",
            "encrypt",
            "0",
        ])
        .status()
        .expect("gpg should run");
    assert!(enc.success(), "encryption subkey add failed");

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
            "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"{brief}\"]\nresign_key_id = \"{brief}\"\n"
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
        &[("GNUPGHOME", &gpg_home_str)],
    );
    assert!(ok, "seal with a live key must work: {msg}");

    std::thread::sleep(std::time::Duration::from_secs(3));
    let (ok, msg) = gpg_fpr_state(&gpg_home_str, &brief);
    assert!(ok, "list keys failed: {msg}");
    assert!(
        msg.contains("pub:e:"),
        "test setup: the signing key must be expired by now: {msg}"
    );

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
    assert!(
        !ok,
        "a bundle whose signer key has expired must stop opening: {msg}"
    );
    assert!(
        msg.contains("EXPKEYSIG") || msg.contains("refusing to open"),
        "the refusal must name the reason: {msg}"
    );
    assert!(!received.exists(), "refused verify must not leave a tree");

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A recipient-side record of what happened. For a governance tool, "who
/// opened this bundle, when, and was the signer pinned" is the question a
/// maintainer will be asked later, and it was unanswerable: verify wrote
/// nothing at all. Both outcomes must be recorded, because a refusal is
/// exactly the event worth keeping.
#[test]
fn test_verify_records_opens_and_refusals() {
    let tmp = scratch("verify-audit");
    let (gpg_home, key_id) = make_test_key();
    let (_src, proj) = project(&tmp, &key_id);
    let gpg_home_str = gpg_home.to_str().unwrap().to_string();

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", &gpg_home_str)],
    );
    assert!(ok, "seal failed: {msg}");

    // Default location: a log beside the output tree.
    let log = tmp.join("oot-verify-log.jsonl");

    let received = tmp.join("received");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
            "--expect-signer",
            &key_id,
        ],
        &proj,
        &[("GNUPGHOME", &gpg_home_str)],
    );
    assert!(ok, "verify failed: {msg}");
    assert!(
        msg.contains("recorded this open"),
        "must say it recorded: {msg}"
    );

    // A refusal, with the log somewhere else.
    let elsewhere = tmp.join("audit").join("opens.jsonl");
    let (ok, _msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            tmp.join("received2").to_str().unwrap(),
            "--expect-signer",
            "DEADBEEFDEADBEEF",
            "--audit-log",
            elsewhere.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", &gpg_home_str)],
    );
    assert!(!ok, "a wrong pin must still be refused");

    let opened: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
    let opened = opened.as_object().expect("one JSON object");
    assert_eq!(opened["event"], "embargo-verified");
    assert_eq!(opened["signer_pinned"], true);
    assert_eq!(opened["signer"], key_id);
    assert_eq!(opened["embargo_until"], "2099-01-01");
    assert!(
        opened["artifact_fnv1a"]
            .as_str()
            .is_some_and(|d| !d.is_empty()),
        "the record must bind to the artifact: {opened:?}"
    );

    let refused: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&elsewhere).unwrap()).unwrap();
    let refused = refused.as_object().expect("one JSON object");
    assert_eq!(refused["event"], "embargo-verify-refused");
    assert!(
        refused["reason"]
            .as_str()
            .is_some_and(|r| r.contains("signer mismatch")),
        "a refusal must say why: {refused:?}"
    );
    assert_eq!(
        refused["artifact_fnv1a"], opened["artifact_fnv1a"],
        "both records must name the same artifact"
    );

    drop_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}
