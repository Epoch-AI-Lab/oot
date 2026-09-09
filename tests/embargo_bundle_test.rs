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
        &["embargo-bundle", "--out", bundle.to_str().unwrap()],
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
        &["embargo-bundle", "--out", bundle.to_str().unwrap()],
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
        &["embargo-bundle", "--out", bundle.to_str().unwrap()],
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
        &["embargo-bundle", "--out", bundle.to_str().unwrap()],
        &proj,
    );
    assert!(!ok, "bundle into existing dir must fail");
    assert!(msg.contains("already exists"), "error must say why: {msg}");

    let _ = std::fs::remove_dir_all(&tmp);
}
