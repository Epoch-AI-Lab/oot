//! Symlink and data-loss regressions for the embargo seal/verify paths.
//!
//! These are the cases where a refusal must NOT become destructive, and
//! where a refusal must not leave a half-built tree behind. Each one plants
//! a symlink or pre-existing data at a path Oot derives from the CLI
//! arguments, then asserts both the refusal and the survival of the canary.

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
    let dir = std::env::temp_dir().join(format!("oot-argcase-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Sealing onto a symlinked artifact path must refuse and leave the link
/// target untouched. Otherwise a planted link redirects the sealed bundle —
/// and, on the guard path, the plaintext — somewhere the operator never
/// asked for.
#[test]
fn test_seal_refuses_symlinked_artifact() {
    let tmp = scratch("artifact");
    let (gpg_home, key_id) = make_test_key();
    let (_src, proj) = project(&tmp, &key_id);
    let gpg_home = gpg_home.to_str().unwrap().to_string();

    let victim = tmp.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "VICTIM DATA").unwrap();
    let artifact = tmp.join("bundle.tar.gpg");
    std::os::unix::fs::symlink(&victim, &artifact).unwrap();

    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(!ok, "seal onto a symlink must refuse");
    // A link to an existing target is caught by the `already exists` check
    // too, so only the dangling variant below distinguishes the two.
    assert!(
        msg.contains("must not be a symlink") || msg.contains("already exists"),
        "error must explain the refusal: {msg}"
    );
    // The link is still a link, and the target keeps exactly its one file.
    assert!(artifact
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_to_string(victim.join("keep.txt")).unwrap(),
        "VICTIM DATA"
    );
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);

    drop_key(Path::new(&gpg_home));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A symlink planted at the derived staging or tar path must refuse the
/// seal. Both names are predictable from `--out`, so this is the plaintext
/// redirection case: the guard must never unlink or write through them.
#[test]
fn test_seal_refuses_symlinked_staging_and_tar() {
    for which in ["staging", "tar"] {
        let tmp = scratch(&format!("stage-{which}"));
        let (gpg_home, key_id) = make_test_key();
        let (_src, proj) = project(&tmp, &key_id);
        let gpg_home = gpg_home.to_str().unwrap().to_string();

        let out_dir = tmp.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let victim = tmp.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("secret-looking.txt"), "PLAININTEXT").unwrap();
        // `bundle.tar.gpg` derives staging `bundle` and tar `bundle.tar`.
        let planted = out_dir.join(if which == "staging" {
            "bundle"
        } else {
            "bundle.tar"
        });
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        let (ok, _msg) = oot_with_env(
            &[
                "embargo-bundle",
                "--out",
                out_dir.join("bundle.tar.gpg").to_str().unwrap(),
            ],
            &proj,
            &[("GNUPGHOME", &gpg_home)],
        );
        assert!(!ok, "{which} symlink must refuse the seal");
        assert!(
            planted.symlink_metadata().is_ok(),
            "{which} symlink must survive"
        );
        assert!(
            !out_dir.join("bundle.tar.gpg").exists(),
            "{which}: no artifact"
        );
        assert_eq!(
            std::fs::read_to_string(victim.join("secret-looking.txt")).unwrap(),
            "PLAININTEXT",
            "{which}: victim untouched"
        );
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);

        drop_key(Path::new(&gpg_home));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

/// Verifying into a symlinked output path must refuse without writing
/// through the link, and must not leave the output tree behind.
#[test]
fn test_verify_refuses_symlinked_output() {
    let tmp = scratch("verifyout");
    let (gpg_home, key_id) = make_test_key();
    let (_src, proj) = project(&tmp, &key_id);
    let gpg_home = gpg_home.to_str().unwrap().to_string();

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(ok, "seal failed: {msg}");

    let victim = tmp.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "VICTIM").unwrap();
    let out = tmp.join("received");
    std::os::unix::fs::symlink(&victim, &out).unwrap();

    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(!ok, "verify into a symlink must refuse");
    assert!(out.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
    assert!(
        msg.contains("must not be a symlink"),
        "error must name the symlink: {msg}"
    );

    drop_key(Path::new(&gpg_home));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A *dangling* symlink at the artifact path is the case `exists()` alone
/// misses: it follows links, so a link to a not-yet-created file looks
/// absent and the seal would write the bundle through it. This is the
/// assertion that actually pins the symlink refusal.
#[test]
fn test_seal_refuses_dangling_symlinked_artifact() {
    let tmp = scratch("dangling");
    let (gpg_home, key_id) = make_test_key();
    let (_src, proj) = project(&tmp, &key_id);
    let gpg_home = gpg_home.to_str().unwrap().to_string();

    // Points into a directory that exists, at a file that does not: the
    // link target is absent, so `Path::exists` reports false.
    let dest_dir = tmp.join("redirect");
    std::fs::create_dir_all(&dest_dir).unwrap();
    let artifact = tmp.join("bundle.tar.gpg");
    std::os::unix::fs::symlink(dest_dir.join("planted.gpg"), &artifact).unwrap();
    assert!(
        !artifact.exists(),
        "test setup: the link target must look absent"
    );

    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(!ok, "seal onto a dangling symlink must refuse");
    assert!(
        msg.contains("must not be a symlink"),
        "error must name the symlink: {msg}"
    );
    // Nothing was written through the link, and the link is intact.
    assert!(
        !dest_dir.join("planted.gpg").exists(),
        "the seal must not write through the symlink"
    );
    assert_eq!(std::fs::read_dir(&dest_dir).unwrap().count(), 0);
    assert!(artifact.symlink_metadata().is_ok(), "symlink must survive");

    drop_key(Path::new(&gpg_home));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The dangling-link cases for the other three Oot-derived paths. Each
/// points at a not-yet-existing file inside a real directory, so
/// `Path::exists` reports false and only the symlink check can catch it.
#[test]
fn test_dangling_symlinks_refused_on_every_derived_path() {
    // (artifact/verify out/plain dir, temp tar) verified separately below.
    let tmp = scratch("dangling3");
    let (gpg_home, key_id) = make_test_key();
    let (_src, proj) = project(&tmp, &key_id);
    let gpg_home = gpg_home.to_str().unwrap().to_string();

    // 1. `--plain` output dir: a dangling link must not be created through.
    let plain_dest = tmp.join("plain-dest");
    std::fs::create_dir_all(&plain_dest).unwrap();
    let plain_out = tmp.join("plain-bundle");
    std::os::unix::fs::symlink(plain_dest.join("planted"), &plain_out).unwrap();
    let (ok, msg) = oot_with_env(
        &[
            "embargo-bundle",
            "--out",
            plain_out.to_str().unwrap(),
            "--plain",
        ],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(!ok, "--plain into a dangling symlink must refuse");
    assert!(
        msg.contains("must not be a symlink"),
        "must name the symlink: {msg}"
    );
    assert_eq!(
        std::fs::read_dir(&plain_dest).unwrap().count(),
        0,
        "--plain must not create through the link"
    );

    // Seal normally so there is an artifact to verify.
    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(ok, "seal failed: {msg}");

    // 2. Verify `--out`: dangling link must not be populated through.
    let recv_dest = tmp.join("recv-dest");
    std::fs::create_dir_all(&recv_dest).unwrap();
    let received = tmp.join("received");
    std::os::unix::fs::symlink(recv_dest.join("planted"), &received).unwrap();
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(!ok, "verify into a dangling symlink must refuse");
    assert!(
        msg.contains("must not be a symlink"),
        "must name the symlink: {msg}"
    );
    assert_eq!(
        std::fs::read_dir(&recv_dest).unwrap().count(),
        0,
        "verify must not write through the link"
    );

    // 3. Verify temp tar: dangling link must not receive plaintext.
    let tar_dest = tmp.join("tar-dest");
    std::fs::create_dir_all(&tar_dest).unwrap();
    let received2 = tmp.join("received2");
    // The temp tar is a sibling of `--out` named `<out-name>.decrypting.tar`.
    std::os::unix::fs::symlink(
        tar_dest.join("planted.tar"),
        tmp.join("received2.decrypting.tar"),
    )
    .unwrap();
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received2.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(!ok, "a dangling temp-tar symlink must refuse the open");
    assert!(
        msg.contains("must not be a symlink"),
        "must name the symlink: {msg}"
    );
    assert_eq!(
        std::fs::read_dir(&tar_dest).unwrap().count(),
        0,
        "plaintext must never be written through the link"
    );
    assert!(
        !received2.exists(),
        "refused verify must not leave an output tree: {msg}"
    );

    drop_key(Path::new(&gpg_home));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The temp-tar path that gpg writes plaintext into must refuse a symlink,
/// dangling or not. A *live* link looks like a stale file to the `exists()`
/// guard and is caught there, so the case that actually exercises
/// `refuse_symlink` is the dangling one, where `Path::exists` reports false.
/// Both must refuse, both must clear the output dir, and neither may write
/// through the link.
#[test]
fn test_verify_temp_tar_symlink_refuses_and_clears_output() {
    for dangling in [true, false] {
        let tmp = scratch(&format!(
            "tmptar-{}",
            if dangling { "dangling" } else { "live" }
        ));
        let (gpg_home, key_id) = make_test_key();
        let (_src, proj) = project(&tmp, &key_id);
        let gpg_home = gpg_home.to_str().unwrap().to_string();

        let artifact = tmp.join("bundle.tar.gpg");
        let (ok, msg) = oot_with_env(
            &["embargo-bundle", "--out", artifact.to_str().unwrap()],
            &proj,
            &[("GNUPGHOME", &gpg_home)],
        );
        assert!(ok, "seal failed: {msg}");

        // The temp tar is `<out-name>.decrypting.tar`, a sibling of `--out`.
        let victim = tmp.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep.txt"), "VICTIM").unwrap();
        let target = if dangling {
            victim.join("not-there.tar")
        } else {
            victim.join("keep.txt")
        };
        let received = tmp.join("received");
        std::os::unix::fs::symlink(&target, tmp.join("received.decrypting.tar")).unwrap();

        let (ok, msg) = oot_with_env(
            &[
                "embargo-verify",
                "--artifact",
                artifact.to_str().unwrap(),
                "--out",
                received.to_str().unwrap(),
            ],
            &proj,
            &[("GNUPGHOME", &gpg_home)],
        );
        assert!(!ok, "a symlinked temp tar must refuse the open");
        assert!(
            msg.contains("must not be a symlink") || msg.contains("stale verify"),
            "refusal must be explained: {msg}"
        );
        assert!(
            !received.exists(),
            "refused verify must not leave an output tree: {msg}"
        );
        assert!(
            tmp.join("received.decrypting.tar")
                .symlink_metadata()
                .is_ok(),
            "the planted symlink must survive"
        );
        // The victim is untouched: one file, and no plaintext landed in it.
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(victim.join("keep.txt")).unwrap(),
            "VICTIM",
            "verify must not write through the symlink"
        );

        // Clearing the real cause makes the retry clean: no stale refusal.
        std::fs::remove_file(tmp.join("received.decrypting.tar")).unwrap();
        let (ok, msg) = oot_with_env(
            &[
                "embargo-verify",
                "--artifact",
                artifact.to_str().unwrap(),
                "--out",
                received.to_str().unwrap(),
            ],
            &proj,
            &[("GNUPGHOME", &gpg_home)],
        );
        assert!(ok, "retry after a symlink refusal must work: {msg}");

        drop_key(Path::new(&gpg_home));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

/// `--plain` writes a directory. A symlink at that path must be refused,
/// not followed: a plain bundle holds the unfiltered private history.
#[test]
fn test_plain_bundle_refuses_symlinked_output_dir() {
    let tmp = scratch("plain");
    let (gpg_home, key_id) = make_test_key();
    let (_src, proj) = project(&tmp, &key_id);
    let gpg_home = gpg_home.to_str().unwrap().to_string();

    let victim = tmp.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "VICTIM").unwrap();
    let out = tmp.join("plain-bundle");
    std::os::unix::fs::symlink(&victim, &out).unwrap();

    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", out.to_str().unwrap(), "--plain"],
        &proj,
        &[("GNUPGHOME", &gpg_home)],
    );
    assert!(!ok, "plain bundle into a symlink must refuse");
    assert!(out.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
    assert!(
        msg.contains("must not be a symlink"),
        "error must name the symlink: {msg}"
    );

    drop_key(Path::new(&gpg_home));
    let _ = std::fs::remove_dir_all(&tmp);
}
