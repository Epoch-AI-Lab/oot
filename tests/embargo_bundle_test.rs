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
        &[
            "embargo-bundle",
            "--out",
            bundle.to_str().unwrap(),
            "--plain",
        ],
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
        &[
            "embargo-bundle",
            "--out",
            bundle.to_str().unwrap(),
            "--plain",
        ],
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
        &[
            "embargo-bundle",
            "--out",
            bundle.to_str().unwrap(),
            "--plain",
        ],
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
        &[
            "embargo-bundle",
            "--out",
            bundle.to_str().unwrap(),
            "--plain",
        ],
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
        &[
            "embargo-bundle",
            "--out",
            bundle.to_str().unwrap(),
            "--plain",
        ],
        &proj,
    );
    assert!(ok, "bundle must survive a bad tag: {msg}");

    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("tag-omitted") && log.contains("bad..tag"),
        "bad tag omission must be audited: {log}"
    );
    let tags = git(&bundle.join("repo"), &["for-each-ref", "refs/tags"]);
    assert!(
        tags.is_empty(),
        "bad tag must not land in the bundle: {tags}"
    );

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

/// A key whose primary signs through a SUBKEY, like most real keys: the
/// VALIDSIG print is the subkey, while `gpg --fingerprint` and
/// `visibility.toml` name the primary. Signing here goes through a signing
/// subkey, which is what made the original pin logic reject the primary
/// fingerprint an operator would actually copy. Returns (home, primary fpr).
fn make_subkey_signing_key() -> (std::path::PathBuf, String) {
    let (home, _primary) = make_test_key();
    let keygen = Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "oot-subkey@example.com",
            "rsa3072",
            "cert",
            "0",
        ])
        .output()
        .expect("gpg should run");
    assert!(
        keygen.status.success(),
        "subkey primary keygen failed: {}",
        String::from_utf8_lossy(&keygen.stderr)
    );
    let list = Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args(["--list-keys", "--with-colons"])
        .output()
        .expect("gpg should run");
    let fpr = String::from_utf8_lossy(&list.stdout)
        .lines()
        .find(|l| l.starts_with("fpr:"))
        .and_then(|l| l.split(':').nth(9))
        .expect("primary keygen must yield a fingerprint")
        .to_string();
    // Signing subkey + encryption subkey, the normal shape.
    for (algo, usage) in [("rsa3072", "sign"), ("rsa3072", "encrypt")] {
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
                algo,
                usage,
                "0",
            ])
            .output()
            .expect("gpg should run");
        assert!(
            add.status.success(),
            "gpg subkey add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
    }
    (home, fpr)
}

/// The fingerprint of the subkey that actually performs signatures for a
/// subkey-signing primary: the `ssb` record whose capability field
/// (index 11) contains `s`. Picking the first `ssb` would return the
/// encryption subkey, which never signs.
fn signing_subkey_fpr(gpg_home: &Path, primary: &str) -> String {
    let out = Command::new("gpg")
        .env("GNUPGHOME", gpg_home)
        .args(["--list-secret-keys", "--with-colons", primary])
        .output()
        .expect("gpg should run");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pending: Option<String> = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        match fields.first() {
            Some(&"ssb") => {
                let can_sign = fields.get(11).is_some_and(|caps| caps.contains('s'));
                pending = if can_sign { Some(String::new()) } else { None };
            }
            Some(&"fpr") => {
                if let Some(slot) = pending.as_mut() {
                    if slot.is_empty() {
                        *slot = fields[9].to_string();
                        return slot.clone();
                    }
                }
            }
            _ => {}
        }
    }
    panic!("no signing subkey fingerprint found");
}

/// Remove a throwaway keyring and stop its agent, so secret keys do not
/// pile up in /tmp across runs.
fn drop_test_key(gpg_home: &Path) {
    let _ = Command::new("gpgconf")
        .env("GNUPGHOME", gpg_home)
        .args(["--kill", "gpg-agent"])
        .status();
    let _ = std::fs::remove_dir_all(gpg_home);
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
    assert!(
        !tmp.join("bundle.tar").exists(),
        "plaintext tar must be gone"
    );

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
    assert!(
        listing.contains("MANIFEST.json"),
        "manifest in tar: {listing}"
    );
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
    let kind = git(
        &extract.join("bundle/repo"),
        &["cat-file", "-t", "refs/tags/v1"],
    );
    assert_eq!(kind, "tag", "tag object must survive the sealed bundle");

    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("embargo-sealed") && log.contains(&key_id),
        "seal event must be audited with the signer: {log}"
    );

    drop_test_key(&gpg_home);
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
    assert!(
        !artifact.exists(),
        "refused seal must not leave an artifact"
    );
    assert!(
        !tmp.join("bundle").exists(),
        "refused seal must not build plaintext"
    );

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
    assert!(msg.contains("needs a signer"), "error must say why: {msg}");
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
        &[
            "--armor",
            "--output",
            pub_key.to_str().unwrap(),
            "--export",
            &key_id,
        ],
    );
    assert!(ok, "export recipient key failed: {msg}");
    let (ok, msg) = gpg_run(&operator_home, &["--import", pub_key.to_str().unwrap()]);
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
    std::fs::write(src.join(".env"), "HANDOFF_TEST=private\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "private fixture"]);

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
    assert!(!tmp.join("bundle").exists());
    assert!(!tmp.join("bundle.tar").exists());

    let signer_pub = tmp.join("signer.pub");
    let (ok, msg) = gpg_run(
        &operator_home,
        &[
            "--output",
            signer_pub.to_str().unwrap(),
            "--export",
            &signer_id,
        ],
    );
    assert!(ok, "export signer key failed: {msg}");
    let (ok, msg) = gpg_run(&recipient_home, &["--import", signer_pub.to_str().unwrap()]);
    assert!(ok, "import signer key failed: {msg}");

    let denied_tar = tmp.join("denied.tar");
    let (ok, msg) = gpg_run(
        &operator_home,
        &[
            "--batch",
            "--status-fd",
            "1",
            "--output",
            denied_tar.to_str().unwrap(),
            "--decrypt",
            artifact.to_str().unwrap(),
        ],
    );
    assert!(
        !ok,
        "sender without recipient secret key must not decrypt: {msg}"
    );
    assert!(
        msg.contains("[GNUPG:] NO_SECKEY"),
        "expected missing recipient secret key: {msg}"
    );
    assert!(!denied_tar.exists());

    let plain_tar = tmp.join("received.tar");
    let (ok, msg) = gpg_run(
        &recipient_home,
        &[
            "--batch",
            "--status-fd",
            "1",
            "--output",
            plain_tar.to_str().unwrap(),
            "--decrypt",
            artifact.to_str().unwrap(),
        ],
    );
    assert!(ok, "recipient must decrypt and verify: {msg}");
    assert!(
        msg.contains(&format!("[GNUPG:] VALIDSIG {signer_id} ")),
        "signature must match sender fingerprint: {msg}"
    );
    assert!(
        msg.contains("[GNUPG:] DECRYPTION_OKAY"),
        "decryption must complete: {msg}"
    );

    let extract = tmp.join("received");
    std::fs::create_dir_all(&extract).unwrap();
    let unpack = Command::new("tar")
        .arg("-xf")
        .arg(&plain_tar)
        .arg("-C")
        .arg(&extract)
        .output()
        .expect("tar should run");
    assert!(
        unpack.status.success(),
        "unpack failed: {}",
        String::from_utf8_lossy(&unpack.stderr)
    );
    let received = extract.join("bundle");
    assert_eq!(
        git(&received.join("repo"), &["rev-parse", "HEAD"]),
        git(&src, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&received.join("repo"), &["show", "HEAD:.env"]),
        "HANDOFF_TEST=private"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(received.join("MANIFEST.json")).unwrap()).unwrap();
    assert_eq!(manifest["recipients"], serde_json::json!([key_id]));
    assert_eq!(manifest["embargo_until"], "2099-01-01");
    assert!(received.join("export-log.jsonl").exists());

    let _ = Command::new("gpgconf")
        .env("GNUPGHOME", &operator_home)
        .args(["--kill", "gpg-agent"])
        .status();
    let _ = Command::new("gpgconf")
        .env("GNUPGHOME", &recipient_home)
        .args(["--kill", "gpg-agent"])
        .status();
    let _ = std::fs::remove_dir_all(&recipient_home);
    drop_test_key(&recipient_home);
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
    assert!(
        !artifact.exists(),
        "refused seal must not leave an artifact"
    );
    assert!(
        !tmp.join("bundle").exists(),
        "refused seal must not build plaintext"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The documented handoff verbatim: a key that signs through a SUBKEY
/// must still open when pinned with the PRIMARY fingerprint the operator
/// reads off `gpg --fingerprint` and writes into `visibility.toml`. The
/// earlier pin compared only VALIDSIG's first field, which is the subkey,
/// so the documented command failed for most real keys.
#[test]
fn test_embargo_verify_pins_primary_fingerprint_of_subkey_signer() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-subkey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    let (gpg_home, primary) = make_subkey_signing_key();
    let subkey = signing_subkey_fpr(&gpg_home, &primary);
    assert_ne!(subkey, primary, "test setup: signing must use a subkey");
    let gpg_home_str = gpg_home.to_str().unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);
    std::fs::write(src.join(".env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "rotate .env secret"]);

    // Policy names the primary, exactly as an operator would.
    std::fs::write(
        proj.join("visibility.toml"),
        format!(
            "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"{primary}\"]\nresign_key_id = \"{primary}\"\n"
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
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(ok, "seal with a subkey signer must work: {msg}");

    // The documented command: pin the full primary fingerprint.
    let received = tmp.join("received");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
            "--expect-signer",
            &primary,
        ],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(
        ok,
        "pinning the primary fingerprint must work for a subkey signer: {msg}"
    );
    // The report names the primary, not the subkey: that is the stable
    // identity an operator records.
    assert!(
        msg.contains(&primary),
        "report must name the primary fingerprint: {msg}"
    );
    assert!(
        git(&received.join("bundle/repo"), &["show", "HEAD:.env"]).contains("supersecret"),
        "unpacked bundle must hold the private blob"
    );

    // The subkey fingerprint and its long key id also pin.
    for (label, pin) in [
        ("subkey fpr", subkey.as_str()),
        ("long key id", &primary[primary.len() - 16..]),
    ] {
        let out = tmp.join(format!("received-{}", label.replace(' ', "-")));
        let (ok, msg) = oot_with_env(
            &[
                "embargo-verify",
                "--artifact",
                artifact.to_str().unwrap(),
                "--out",
                out.to_str().unwrap(),
                "--expect-signer",
                pin,
            ],
            &proj,
            &[("GNUPGHOME", gpg_home_str)],
        );
        assert!(ok, "pin by {label} must work: {msg}");
    }

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The plaintext guard must never delete data the user already had at the
/// predicted staging/tar names. A refusal that also destroys the operator's
/// files is worse than the refusal itself: `--out notes.tar.gpg` sits beside
/// a real `notes/` release directory and a prebuilt `notes.tar`.
#[test]
fn test_embargo_seal_never_deletes_preexisting_paths() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-preexist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    let (gpg_home, key_id) = make_test_key();
    let gpg_home_str = gpg_home.to_str().unwrap();

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

    // The artifact lives in its own dir so the predicted staging/tar names
    // are `out/notes` and `out/notes.tar`, both pre-created with data.
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(out_dir.join("notes/important")).unwrap();
    std::fs::write(out_dir.join("notes/keepme.txt"), "IRREPLACEABLE NOTES").unwrap();
    std::fs::write(
        out_dir.join("notes/important/data.txt"),
        "IRREPLACEABLE DATA",
    )
    .unwrap();
    std::fs::write(out_dir.join("notes.tar"), "PRECIOUS PREBUILT TARBALL").unwrap();

    let artifact = out_dir.join("notes.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(!ok, "seal beside existing staging/tar paths must refuse");
    assert!(
        msg.contains("already exists"),
        "error must name the conflict: {msg}"
    );

    // Every pre-existing path survives with its contents.
    assert_eq!(
        std::fs::read_to_string(out_dir.join("notes/keepme.txt")).unwrap(),
        "IRREPLACEABLE NOTES"
    );
    assert_eq!(
        std::fs::read_to_string(out_dir.join("notes/important/data.txt")).unwrap(),
        "IRREPLACEABLE DATA"
    );
    assert_eq!(
        std::fs::read_to_string(out_dir.join("notes.tar")).unwrap(),
        "PRECIOUS PREBUILT TARBALL"
    );
    assert!(
        !artifact.exists(),
        "refused seal must not leave an artifact"
    );

    // Clean up the obstruction: the same command now succeeds.
    std::fs::remove_dir_all(out_dir.join("notes")).unwrap();
    std::fs::remove_file(out_dir.join("notes.tar")).unwrap();
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(ok, "seal must work once the paths are free: {msg}");
    assert!(artifact.exists(), "artifact must exist");
    assert!(
        !out_dir.join("notes").exists() && !out_dir.join("notes.tar").exists(),
        "successful seal must still clean up its own plaintext"
    );

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A hostile tar must be refused by name, not unpacked: `..` traversal,
/// absolute member names, and non-regular member types all fail the open
/// with a named reason, leaving no tree.
#[test]
fn test_embargo_verify_refuses_hostile_tar_members() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-hostile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    let (gpg_home, key_id) = make_test_key();
    let gpg_home_str = gpg_home.to_str().unwrap();
    sealable_project(&tmp, &src, &proj, &key_id);

    // Build a hostile tar per case, then sign+encrypt it to the recipient
    // so only the tar policy can refuse.
    let outside = tmp.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("canary.txt"), "CANARY").unwrap();
    for case in ["dotdot", "absolute", "fifo"] {
        let stage = tmp.join(format!("stage-{case}"));
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(stage.join("bundle")).unwrap();
        std::fs::write(
            stage.join("bundle/MANIFEST.json"),
            "{\"embargo_until\":\"2099-01-01\",\"recipients\":[],\"changes\":[]}",
        )
        .unwrap();
        if case == "fifo" {
            // A FIFO member: never valid in a code bundle.
            let fifo = stage.join("bundle/pipe");
            let made = Command::new("mkfifo").arg(&fifo).status();
            assert!(made.map(|s| s.success()).unwrap_or(false), "mkfifo failed");
        } else {
            // Renamed below into a traversal or absolute member name.
            std::fs::write(stage.join("bundle/pwned.txt"), "PWNED").unwrap();
        }
        let tar_path = tmp.join(format!("hostile-{case}.tar"));
        let _ = std::fs::remove_file(&tar_path);
        let created = Command::new("tar")
            .arg("--create")
            .arg("--file")
            .arg(&tar_path)
            .arg("--directory")
            .arg(&stage)
            .arg("--format")
            .arg("gnu")
            .arg("--")
            .arg("bundle")
            .status();
        assert!(
            created.map(|s| s.success()).unwrap_or(false),
            "tar create failed"
        );
        if case != "fifo" {
            // `-P` keeps the crafted name verbatim: `--transform` rewrites
            // the member to a traversal or absolute path that stock tar
            // would otherwise strip at creation time.
            let escape = if case == "dotdot" {
                "bundle/../../ESCAPE_DOTDOT.txt"
            } else {
                "/tmp/opencode/oot-absolute-escape.txt"
            };
            let appended = Command::new("tar")
                .arg("--append")
                .arg("--absolute-names")
                .arg("--file")
                .arg(&tar_path)
                .arg("--transform")
                .arg(format!("s|bundle/pwned.txt|{escape}|"))
                .arg("--directory")
                .arg(&stage)
                .arg("bundle/pwned.txt")
                .status();
            assert!(
                appended.map(|s| s.success()).unwrap_or(false),
                "tar append failed"
            );
        }

        let artifact = tmp.join(format!("hostile-{case}.tar.gpg"));
        let sealed = Command::new("gpg")
            .env("GNUPGHOME", gpg_home_str)
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
            .status();
        assert!(
            sealed.map(|s| s.success()).unwrap_or(false),
            "gpg seal failed"
        );

        let received = tmp.join(format!("received-{case}"));
        let (ok, msg) = oot_with_env(
            &[
                "embargo-verify",
                "--artifact",
                artifact.to_str().unwrap(),
                "--out",
                received.to_str().unwrap(),
            ],
            &proj,
            &[("GNUPGHOME", gpg_home_str)],
        );
        assert!(!ok, "{case} bundle must be refused");
        assert!(
            msg.contains("refusing to open"),
            "{case} error must say it refused: {msg}"
        );
        assert!(!received.exists(), "{case} must not leave a tree");
        // The canary is a real file the test owns: extraction must not
        // touch, truncate, or replace anything outside the output dir.
        assert_eq!(
            std::fs::read_to_string(outside.join("canary.txt")).unwrap(),
            "CANARY",
            "{case} must not modify anything outside the output dir"
        );
        assert!(
            !tmp.join("ESCAPE.txt").exists() && !tmp.join("ESCAPE_DOTDOT.txt").exists(),
            "{case} must not write an escaping member"
        );
    }
    assert!(
        !std::path::Path::new("/tmp/opencode/oot-absolute-escape.txt").exists(),
        "absolute member must not be written"
    );

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A seal that cannot even write its plaintext must not report success:
/// exit nonzero, no artifact to ship, and the failure named. Forced by
/// sealing into a read-only parent, so `gpg --output` itself fails after
/// the staging dir was built.
#[test]
fn test_embargo_seal_fails_when_plaintext_cannot_be_written() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    let (gpg_home, key_id) = make_test_key();
    sealable_project(&tmp, &src, &proj, &key_id);

    // A read-only parent: the plaintext staging dir cannot even be created,
    // and any artifact write fails too.
    let ro = tmp.join("readonly");
    std::fs::create_dir_all(&ro).unwrap();
    #[cfg(unix)]
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
    assert!(!ok, "a seal that cannot finish must not report success");
    assert!(!msg.contains("sealed"), "must not claim sealed: {msg}");
    assert!(!artifact.exists(), "no artifact may survive a failed seal");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The recipient half of the handoff: `embargo-verify` decrypts, requires
/// a valid signature, pins the signer, and unpacks the same tree the
/// sender sealed. Transport stays out of band; Oot only seals and opens.
#[test]
fn test_embargo_verify_opens_sealed_bundle() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    let (gpg_home, key_id) = make_test_key();
    let gpg_home_str = gpg_home.to_str().unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);
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
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(ok, "sealed bundle failed: {msg}");

    // Full fingerprint pins the signer; the unpacked tree matches.
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
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(ok, "verify failed: {msg}");
    assert!(
        msg.contains("verified embargo bundle"),
        "must report: {msg}"
    );
    assert!(msg.contains(&key_id), "must name the signer: {msg}");
    assert_eq!(
        git(&received.join("bundle/repo"), &["show", "HEAD:.env"]).trim(),
        "API_KEY=supersecret"
    );
    let manifest = std::fs::read_to_string(received.join("bundle/MANIFEST.json")).unwrap();
    assert!(manifest.contains("2099-01-01"), "manifest date: {manifest}");
    assert!(manifest.contains(&key_id), "manifest recipient: {manifest}");

    // A trailing key-id suffix pins too.
    let short = &key_id[key_id.len().saturating_sub(16)..];
    let received_short = tmp.join("received-short");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received_short.to_str().unwrap(),
            "--expect-signer",
            short,
        ],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(ok, "short key-id verify failed: {msg}");

    // Wrong signer refuses and leaves no tree behind.
    let denied = tmp.join("denied");
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            denied.to_str().unwrap(),
            "--expect-signer",
            "DEADBEEFDEADBEEF",
        ],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(!ok, "wrong signer must fail");
    assert!(msg.contains("signer mismatch"), "must say why: {msg}");
    assert!(!denied.exists(), "refused verify must not leave a tree");

    // Existing output directory refuses.
    let (ok, msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(!ok, "existing out dir must fail");
    assert!(msg.contains("already exists"), "must say why: {msg}");

    drop_test_key(&gpg_home);
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

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Set up the smallest sealable project: one commit plus one secret commit,
/// policy naming `key_id` as recipient and signer. Returns (src, proj).
fn sealable_project(tmp: &Path, src: &Path, proj: &Path, key_id: &str) {
    std::fs::create_dir_all(src).unwrap();
    std::fs::create_dir_all(proj).unwrap();
    git(src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(src, &["add", "."]);
    git(src, &["commit", "-m", "base"]);
    std::fs::write(src.join(".env"), "API_KEY=supersecret\n").unwrap();
    git(src, &["add", "."]);
    git(src, &["commit", "-m", "rotate .env secret"]);
    std::fs::write(
        proj.join("visibility.toml"),
        format!(
            "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"{key_id}\"]\nresign_key_id = \"{key_id}\"\n"
        ),
    )
    .unwrap();
    assert!(oot(&["init"], proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], proj);
    assert!(ok, "import failed: {msg}");
    let _ = tmp;
}

/// A flipped byte anywhere in the sealed artifact must fail the open and
/// leave neither a tree nor a plaintext tar behind.
#[test]
fn test_embargo_verify_refuses_tampered_artifact() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-tamper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let (gpg_home, key_id) = make_test_key();
    sealable_project(&tmp, &src, &proj, &key_id);
    let gpg_home_str = gpg_home.to_str().unwrap();

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(ok, "sealed bundle failed: {msg}");

    let mut bytes = std::fs::read(&artifact).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&artifact, &bytes).unwrap();

    let received = tmp.join("received");
    let (ok, _msg) = oot_with_env(
        &[
            "embargo-verify",
            "--artifact",
            artifact.to_str().unwrap(),
            "--out",
            received.to_str().unwrap(),
        ],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(!ok, "tampered artifact must fail");
    assert!(!received.exists(), "failed verify must not leave a tree");
    assert!(
        !tmp.join("received.decrypting.tar").exists(),
        "failed verify must not leave plaintext"
    );

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Decrypting with the wrong keyring (no recipient secret key) fails the
/// open through the CLI path, not just raw gpg.
#[test]
fn test_embargo_verify_refuses_wrong_keyring() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-wrongkey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let (gpg_home, key_id) = make_test_key();
    let (other_home, _other_id) = make_test_key();
    sealable_project(&tmp, &src, &proj, &key_id);

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home.to_str().unwrap())],
    );
    assert!(ok, "sealed bundle failed: {msg}");

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
        &[("GNUPGHOME", other_home.to_str().unwrap())],
    );
    assert!(!ok, "wrong-keyring verify must fail");
    assert!(msg.contains("gpg decrypt failed"), "must say why: {msg}");
    assert!(!received.exists(), "failed verify must not leave a tree");

    drop_test_key(&gpg_home);
    drop_test_key(&other_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Missing paths and directories are not bundle artifacts. No GPG needed.
#[test]
fn test_embargo_verify_refuses_nonfile_artifact() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-nonfile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    let (ok, msg) = oot(
        &[
            "embargo-verify",
            "--artifact",
            tmp.join("does-not-exist.tar.gpg").to_str().unwrap(),
            "--out",
            tmp.join("received").to_str().unwrap(),
        ],
        &proj,
    );
    assert!(!ok, "missing artifact must fail");
    assert!(
        msg.contains("no such bundle artifact"),
        "must say why: {msg}"
    );

    let dir_artifact = tmp.join("plain-dir");
    std::fs::create_dir_all(&dir_artifact).unwrap();
    let (ok, msg) = oot(
        &[
            "embargo-verify",
            "--artifact",
            dir_artifact.to_str().unwrap(),
            "--out",
            tmp.join("received-dir").to_str().unwrap(),
        ],
        &proj,
    );
    assert!(!ok, "directory artifact must fail");
    assert!(msg.contains("not a bundle file"), "must say why: {msg}");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Short signer pins (< 16 hex chars after normalization) are rejected
/// before anything is created.
#[test]
fn test_embargo_verify_refuses_short_signer() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-shortsig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let (gpg_home, key_id) = make_test_key();
    sealable_project(&tmp, &src, &proj, &key_id);
    let gpg_home_str = gpg_home.to_str().unwrap();

    let artifact = tmp.join("bundle.tar.gpg");
    let (ok, msg) = oot_with_env(
        &["embargo-bundle", "--out", artifact.to_str().unwrap()],
        &proj,
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(ok, "sealed bundle failed: {msg}");

    for pin in ["ABC", "DEADBEEF"] {
        let received = tmp.join(format!("received-{pin}"));
        let (ok, msg) = oot_with_env(
            &[
                "embargo-verify",
                "--artifact",
                artifact.to_str().unwrap(),
                "--out",
                received.to_str().unwrap(),
                "--expect-signer",
                pin,
            ],
            &proj,
            &[("GNUPGHOME", gpg_home_str)],
        );
        assert!(!ok, "short pin {pin} must fail");
        assert!(msg.contains("at least 16 hex chars"), "must say why: {msg}");
        assert!(!received.exists(), "refused pin must not create output");
    }

    // A pin that is not hex is refused as such, rather than being filtered
    // down to whatever hex it happens to contain: a 39-character pin with
    // junk in the middle used to compare as 16 characters.
    for pin in [
        "not-a-key!!",
        "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
    ] {
        let received = tmp.join("received-junk");
        let _ = std::fs::remove_dir_all(&received);
        let (ok, msg) = oot_with_env(
            &[
                "embargo-verify",
                "--artifact",
                artifact.to_str().unwrap(),
                "--out",
                received.to_str().unwrap(),
                "--expect-signer",
                pin,
            ],
            &proj,
            &[("GNUPGHOME", gpg_home_str)],
        );
        assert!(!ok, "junk pin {pin} must fail");
        assert!(msg.contains("must be a hex key id"), "must say why: {msg}");
        assert!(!received.exists(), "refused pin must not create output");
    }

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Encrypted-but-unsigned input decrypts yet has no signature: the open
/// must refuse it.
#[test]
fn test_embargo_verify_refuses_unsigned_bundle() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-nosig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let (gpg_home, key_id) = make_test_key();
    sealable_project(&tmp, &src, &proj, &key_id);
    let gpg_home_str = gpg_home.to_str().unwrap();

    let payload = tmp.join("payload.tar");
    let tar_out = Command::new("tar")
        .args(["-cf"])
        .arg(&payload)
        .arg("-C")
        .arg(&src)
        .arg("--")
        .arg("README.md")
        .env("GNUPGHOME", gpg_home_str)
        .output()
        .expect("tar should run");
    assert!(tar_out.status.success(), "tar payload failed");
    let artifact = tmp.join("unsigned.tar.gpg");
    let enc = Command::new("gpg")
        .env("GNUPGHOME", gpg_home_str)
        .args([
            "--batch",
            "--yes",
            "--trust-model",
            "always",
            "--encrypt",
            "--recipient",
            &key_id,
            "--output",
            artifact.to_str().unwrap(),
            "--",
            payload.to_str().unwrap(),
        ])
        .output()
        .expect("gpg should run");
    assert!(enc.status.success(), "unsigned encrypt failed");

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
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(!ok, "unsigned bundle must fail");
    assert!(msg.contains("no valid signature"), "must say why: {msg}");
    assert!(!received.exists(), "refused verify must not leave a tree");

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A named signer with no secret key in the keyring refuses the seal
/// before any plaintext exists — no artifact, no staging dir, no tar.
#[test]
fn test_embargo_seal_refuses_missing_signer_key() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-nosignerkey-{}", std::process::id()));
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    let (gpg_home, key_id) = make_test_key();
    let gpg_home_str = gpg_home.to_str().unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);
    std::fs::write(
        proj.join("visibility.toml"),
        format!(
            "private_paths = [\".env\"]\nembargo_until = \"2099-01-01\"\nprivate_branches = []\nembargo_recipients = [\"{key_id}\"]\nresign_key_id = \"ghost-signer@example.com\"\n"
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
        &[("GNUPGHOME", gpg_home_str)],
    );
    assert!(!ok, "seal without a signer key must fail");
    assert!(
        msg.contains("no signing key"),
        "error must name the cause: {msg}"
    );
    assert!(
        !artifact.exists(),
        "refused seal must not leave an artifact"
    );
    assert!(
        !tmp.join("bundle").exists(),
        "refused seal must not stage plaintext"
    );
    assert!(
        !tmp.join("bundle.tar").exists(),
        "refused seal must not tar plaintext"
    );

    drop_test_key(&gpg_home);
    let _ = std::fs::remove_dir_all(&tmp);
}
