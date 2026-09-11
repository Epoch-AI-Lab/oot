//! Embargo bundle: maintainers get full history under embargo while the
//! public export still refuses. Bundle holds the secrets change that public
//! export drops, manifest lists recipients, empty list errors.

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

#[test]
fn test_embargo_bundle_holds_what_public_drops() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let public = tmp.join("public");
    let bundle = tmp.join("bundle");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    std::fs::write(src.join(".env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "rotate .env secret"]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"alice@example.com\", \"bob@example.com\"]\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    let (ok, msg) = oot(&["embargo-status"], &proj);
    assert!(ok, "status failed: {msg}");
    assert!(msg.contains("held"), "status must say held: {msg}");
    assert!(msg.contains("2099-01-01"), "status must show date: {msg}");
    assert!(
        msg.contains("2 recipients"),
        "status must count recipients: {msg}"
    );

    let (ok, msg) = oot(&["export", "--out", public.to_str().unwrap()], &proj);
    assert!(!ok, "public export under embargo must fail");
    assert!(msg.contains("embargo"), "error must mention embargo: {msg}");

    let (ok, msg) = oot(
        &["embargo-bundle", "--out", bundle.to_str().unwrap(), "--plain"],
        &proj,
    );
    assert!(ok, "bundle failed: {msg}");

    // Bundle holds the secrets blob that public export drops.
    let secret = git(&bundle.join("repo"), &["show", "HEAD:.env"]);
    assert!(
        secret.contains("supersecret"),
        "bundle must hold the secret blob"
    );

    // Branch refs resolve, HEAD works.
    let head = git(&bundle.join("repo"), &["rev-parse", "main"]);
    assert!(!head.is_empty(), "bundle repo must point main");

    // Sidecars travel with the bundle, and the copy holds this run.
    assert!(bundle.join("MANIFEST.json").exists(), "missing MANIFEST");
    assert!(
        bundle.join("export-log.jsonl").exists(),
        "missing export log copy"
    );
    let bundle_log = std::fs::read_to_string(bundle.join("export-log.jsonl")).unwrap();
    assert!(
        bundle_log.contains("embargo-bundle"),
        "bundle copy must hold this run: {bundle_log}"
    );

    let manifest = std::fs::read_to_string(bundle.join("MANIFEST.json")).unwrap();
    assert!(
        manifest.contains("alice@example.com"),
        "manifest must list recipients: {manifest}"
    );
    assert!(
        manifest.contains("bob@example.com"),
        "manifest must list recipients: {manifest}"
    );
    assert!(
        manifest.contains("2099-01-01"),
        "manifest must show date: {manifest}"
    );

    // Message names a private path, so warn plus log, not scrub.
    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("message-leak-warn"),
        "missing leak warn: {log}"
    );
    assert!(
        log.contains("embargo-bundle"),
        "missing bundle event: {log}"
    );
    assert!(
        log.contains("alice@example.com"),
        "bundle event must name recipients: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_embargo_bundle_refuses_empty_recipients() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let bundle = tmp.join("bundle");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    let (ok, msg) = oot(
        &["embargo-bundle", "--out", bundle.to_str().unwrap(), "--plain"],
        &proj,
    );
    assert!(!ok, "empty recipients must fail");
    assert!(
        msg.contains("embargo_recipients"),
        "error must name the field: {msg}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_embargo_bundle_refuses_without_active_embargo() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-nohold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let bundle = tmp.join("bundle");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    // Past date means lifted, no bundle.
    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nembargo_until = \"2000-01-01\"\nprivate_branches = []\nembargo_recipients = [\"alice@example.com\"]\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    let (ok, msg) = oot(&["embargo-status"], &proj);
    assert!(ok, "status failed: {msg}");
    assert!(msg.contains("lifted"), "status must say lifted: {msg}");

    let (ok, msg) = oot(
        &["embargo-bundle", "--out", bundle.to_str().unwrap(), "--plain"],
        &proj,
    );
    assert!(!ok, "bundle without active embargo must fail");
    assert!(
        msg.contains("no active embargo"),
        "error must say why: {msg}"
    );

    // Existing dir refuses too.
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"alice@example.com\"]\n",
    )
    .unwrap();
    let (ok, msg) = oot(
        &["embargo-bundle", "--out", bundle.to_str().unwrap(), "--plain"],
        &proj,
    );
    assert!(!ok, "bundle into existing dir must fail");
    assert!(msg.contains("already exists"), "error must say why: {msg}");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// One invalid tag must not sink a maintainer bundle: the bad tag is
/// omitted with an audit entry, the rest of the bundle ships.
#[test]
fn test_embargo_bundle_survives_invalid_tag() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-badtag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let bundle = tmp.join("bundle");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"alice@example.com\"]\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    // Plant a tag with a refname git itself would refuse, pointing at the
    // imported change, directly into the store's tag records.
    let head_id =
        std::fs::read_to_string(proj.join(".oot/refs/main")).expect("branch head change id");
    std::fs::write(proj.join(".oot/tags/bad..tag"), head_id.trim()).unwrap();

    let (ok, msg) = oot(
        &["embargo-bundle", "--out", bundle.to_str().unwrap(), "--plain"],
        &proj,
    );
    assert!(ok, "bundle must survive a bad tag: {msg}");

    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("tag-omitted") && log.contains("bad..tag"),
        "bad tag omission must be audited: {log}"
    );
    let tags = git(&bundle.join("repo"), &["for-each-ref", "refs/tags"]);
    assert!(tags.is_empty(), "bad tag must not land in the bundle: {tags}");

    // The working tree is still populated despite the tag omission.
    assert!(
        bundle.join("repo/README.md").exists(),
        "bundle working tree must be checked out"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Like `oot`, but with extra env (used to seal with a throwaway key).
fn oot_with_env(args: &[&str], cwd: &Path, extra_env: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(cwd);
    for (k, v) in extra_env {
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

/// Make a throwaway GPG home with one signing key. Fast (about 0.1s),
/// no passphrase. Unique per call: tests run concurrently in one process.
fn make_test_key() -> (std::path::PathBuf, String) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static KEY_SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = KEY_SEQ.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir()
        .join(format!("oot-embargo-gpg-{}", std::process::id()))
        .join(format!("key-{seq}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let out = Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "oot-test@example.com",
            "ed25519",
            "sign",
            "0",
        ])
        .output()
        .expect("gpg should run");
    assert!(
        out.status.success(),
        "gpg keygen failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let list = Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args(["--list-secret-keys", "--with-colons"])
        .output()
        .expect("gpg should run");
    let text = String::from_utf8_lossy(&list.stdout);
    let fpr = text
        .lines()
        .find(|l| l.starts_with("fpr:"))
        .and_then(|l| l.split(':').nth(9))
        .expect("keygen must yield a fingerprint")
        .to_string();
    assert!(!fpr.is_empty(), "empty fingerprint");
    // The primary is sign-only; sealing needs an encryption subkey.
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
    assert!(
        add.status.success(),
        "gpg add-key failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    (home, fpr)
}

fn gpg_run(gpg_home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("gpg")
        .env("GNUPGHOME", gpg_home)
        .args(args)
        .output()
        .expect("gpg should run");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The default bundle is sealed: one gpg artifact, sign+encrypted to the
/// recipients, with no plaintext left behind. Decrypting yields the same
/// tree the plain bundle used to write.
#[test]
fn test_embargo_bundle_seals_to_gpg_artifact() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-seal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    let (gpg_home, key_id) = make_test_key();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);
    // Annotated tag on the clean prefix: the tag object must ride along.
    git(&src, &["tag", "-a", "v1", "-m", "tag on base"]);
    std::fs::write(src.join(".env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "rotate .env secret"]);

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
    assert!(ok, "sealed bundle failed: {msg}");

    // The artifact exists; no plaintext staging dir or tar survived.
    assert!(artifact.exists(), "sealed artifact must exist");
    assert!(!tmp.join("bundle").exists(), "staging dir must be gone");
    assert!(!tmp.join("bundle.tar").exists(), "plaintext tar must be gone");

    // Decrypt (verifies the signature too) and inspect the tarball.
    let plain_tar = tmp.join("plain.tar");
    let (ok, msg) = gpg_run(
        &gpg_home,
        &[
            "--batch",
            "--yes",
            "--decrypt",
            "--output",
            plain_tar.to_str().unwrap(),
            artifact.to_str().unwrap(),
        ],
    );
    assert!(ok, "decrypt must succeed with a recipient key: {msg}");
    let listing = Command::new("tar")
        .args(["-tf"])
        .arg(&plain_tar)
        .output()
        .expect("tar should run");
    assert!(
        listing.status.success(),
        "tar listing failed: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(listing.contains("MANIFEST.json"), "manifest in tar: {listing}");
    assert!(listing.contains("repo/"), "repo in tar: {listing}");

    // The annotated tag object survives the tarball too.
    let extract = tmp.join("extracted");
    std::fs::create_dir_all(&extract).unwrap();
    let extract_out = Command::new("tar")
        .args(["-xf"])
        .arg(&plain_tar)
        .arg("-C")
        .arg(&extract)
        .output()
        .expect("tar should run");
    assert!(
        extract_out.status.success(),
        "tar extract failed: {}",
        String::from_utf8_lossy(&extract_out.stderr)
    );
    let kind = git(&extract.join("bundle/repo"), &["cat-file", "-t", "refs/tags/v1"]);
    assert_eq!(kind, "tag", "tag object must survive the sealed bundle");

    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("embargo-sealed") && log.contains(&key_id),
        "seal event must be audited with the signer: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// A recipient with no key in the keyring refuses the seal before any
/// plaintext is written.
#[test]
fn test_embargo_bundle_seal_refuses_unresolvable_recipient() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-nokey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    // An empty keyring: nothing resolves.
    let gpg_home = tmp.join("gpg-home");
    std::fs::create_dir_all(&gpg_home).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"ghost@example.com\"]\nresign_key_id = \"AA00BB11\"\n",
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
    assert!(!ok, "seal without the recipient key must fail");
    assert!(
        msg.contains("unusable key for") && msg.contains("ghost@example.com"),
        "error must name the unresolved recipient: {msg}"
    );
    assert!(!artifact.exists(), "refused seal must not leave an artifact");
    assert!(!tmp.join("bundle").exists(), "refused seal must not build plaintext");

    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("seal-refused"),
        "refusal must be audited: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// No signer configured: the seal refuses before touching gpg.
#[test]
fn test_embargo_bundle_seal_needs_signer() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-nosign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
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
        "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"alice@example.com\"]\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
    );
    assert!(!ok, "seal without a signer must fail");
    assert!(
        msg.contains("needs a signer"),
        "error must say why: {msg}"
    );
    assert!(!artifact.exists(), "failed seal must not leave an artifact");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The normal recipient workflow: the operator imports a public key with
/// no ownertrust. The seal must still work — the policy names the
/// recipient, which is the authorization — and must not depend on the
/// signer's own key being the recipient.
#[test]
fn test_embargo_bundle_seal_accepts_untrusted_imported_key() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-untrust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    // Recipient key: made in one keyring, exported, imported (no ownertrust)
    // into the operator keyring that seals the bundle.
    let (recipient_home, key_id) = make_test_key();
    let operator_home = tmp.join("operator-gnupg");
    std::fs::create_dir_all(&operator_home).unwrap();
    let pub_key = tmp.join("recipient.pub");
    let (ok, msg) = gpg_run(
        &recipient_home,
        &["--armor", "--output", pub_key.to_str().unwrap(), "--export", &key_id],
    );
    assert!(ok, "export recipient key failed: {msg}");
    let (ok, msg) = gpg_run(
        &operator_home,
        &["--import", pub_key.to_str().unwrap()],
    );
    assert!(ok, "import recipient key failed: {msg}");
    // The operator signs with their own key, not the recipient's.
    let (ok, msg) = gpg_run(
        &operator_home,
        &[
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "operator@example.com",
            "ed25519",
            "sign",
            "0",
        ],
    );
    assert!(ok, "operator keygen failed: {msg}");
    let signer_id = {
        let (ok, msg) = gpg_run(&operator_home, &["--list-secret-keys", "--with-colons"]);
        assert!(ok, "list secret keys failed: {msg}");
        msg.lines()
            .find(|l| l.starts_with("fpr:"))
            .and_then(|l| l.split(':').nth(9))
            .expect("keygen must yield a fingerprint")
            .to_string()
    };

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    std::fs::write(
        proj.join("visibility.toml"),
        format!(
            "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"{key_id}\"]\nresign_key_id = \"{signer_id}\"\n"
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
        &[("GNUPGHOME", operator_home.to_str().unwrap())],
    );
    assert!(ok, "seal to an untrusted imported key must work: {msg}");
    assert!(artifact.exists(), "sealed artifact must exist");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// An unusable recipient key (here: expired; the same refusal covers
/// revoked and disabled) refuses before any plaintext exists.
#[test]
fn test_embargo_bundle_seal_refuses_expired_key() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-expired-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    // Key that expires two seconds after creation; sleep past it so the
    // keyring reports pub:e. No encryption subkey needed: the pre-flight
    // must refuse before any encryption is attempted.
    let gpg_home = tmp.join("gnupg");
    std::fs::create_dir_all(&gpg_home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gpg_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (ok, msg) = gpg_run(
        &gpg_home,
        &[
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "expired@example.com",
            "ed25519",
            "sign",
            "seconds=2",
        ],
    );
    assert!(ok, "expired key generation failed: {msg}");
    let key_id = {
        let (ok, msg) = gpg_run(&gpg_home, &["--list-keys", "--with-colons"]);
        assert!(ok, "list keys failed: {msg}");
        msg.lines()
            .find(|l| l.starts_with("fpr:"))
            .and_then(|l| l.split(':').nth(9))
            .expect("keygen must yield a fingerprint")
            .to_string()
    };
    std::thread::sleep(std::time::Duration::from_secs(3));
    let (ok, msg) = gpg_run(&gpg_home, &["--list-keys", "--with-colons"]);
    assert!(ok, "list keys failed: {msg}");
    assert!(
        msg.contains("pub:e:"),
        "test setup: key must show as expired: {msg}"
    );

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

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(!ok, "seal to an expired key must fail");
    assert!(
        msg.contains("unusable key for") && msg.contains("expired"),
        "error must name the expired key: {msg}"
    );
    assert!(!artifact.exists(), "refused seal must not leave an artifact");
    assert!(!tmp.join("bundle").exists(), "refused seal must not build plaintext");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// `--signer` overrides `resign_key_id` (here: the policy leaves it unset).
#[test]
fn test_embargo_bundle_seal_signer_override() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-signer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    let (gpg_home, key_id) = make_test_key();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);

    std::fs::write(
        proj.join("visibility.toml"),
        format!(
            "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"{key_id}\"]\n"
        ),
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-bundle",
            "--out",
            artifact.to_str().unwrap(),
            "--signer",
            &key_id,
        ],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(ok, "seal with --signer override must work: {msg}");
    assert!(artifact.exists(), "sealed artifact must exist");

    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("embargo-sealed") && log.contains(&key_id),
        "seal event must name the override signer: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
