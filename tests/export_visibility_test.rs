//! Visibility-filtered export: secrets must not survive into an export,
//! neither as commits nor inside descendant trees. Every withholding decision
//! must land in the export log before anything can be pushed anywhere.

use std::path::{Path, PathBuf};
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

/// Runs an oot subcommand; returns (success, stdout+stderr).
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

/// A three-commit history: clean base, a secret commit, a clean follow-up
/// whose tree still contains the secret file.
fn build_fixture(path: &PathBuf) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "-b", "main"]);
    std::fs::write(path.join("README.md"), "v1\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "base"]);
}

/// Adds the secret commit and a clean follow-up; returns the .env blob sha.
fn add_secret_and_followup(path: &Path) -> String {
    std::fs::write(path.join(".env"), "API_KEY=supersecret\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "add config"]);
    let env_blob = git(path, &["rev-parse", "HEAD:.env"]);

    std::fs::write(path.join("README.md"), "v2\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "docs update"]);
    env_blob
}

#[test]
fn test_filtered_export_strips_private_paths_everywhere() {
    let tmp = std::env::temp_dir().join(format!("oot-filter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    build_fixture(&src);
    let env_blob = add_secret_and_followup(&src);
    let orig_head = git(&src, &["rev-parse", "main"]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nprivate_branches = []\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    // 1. What a push would actually transfer must not contain the secret.
    // The export shares the store's odb via alternates, so probing it
    // directly would see unreferenced objects; a --no-local clone receives
    // exactly the reachable object set, same as a push to GitHub.
    let clone = tmp.join("clone");
    let _ = Command::new("git")
        .args([
            "clone",
            "--no-local",
            "--quiet",
            out.to_str().unwrap(),
            clone.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let check = Command::new("git")
        .arg("-C")
        .arg(&clone)
        .args(["cat-file", "-e", &env_blob])
        .output()
        .unwrap();
    assert!(
        !check.status.success(),
        "secret blob {env_blob} is reachable from the export"
    );

    // 2. No exported tree contains .env.
    let names = git(&out, &["log", "--all", "--name-only", "--format="]);
    assert!(!names.contains(".env"), ".env appears in export: {names}");

    // 3. Clean content survived: latest README reachable at head.
    let readme = git(&out, &["show", "main:README.md"]);
    assert_eq!(readme, "v2");

    // 4. History was rewritten (rebuilt), not copied: head differs.
    let exp_head = git(&out, &["rev-parse", "main"]);
    assert_ne!(exp_head, orig_head, "filtered export reused original SHAs");

    // 5. The audit log recorded the withholding decision.
    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(log.contains("withheld-change"), "log missing entry: {log}");
    assert!(log.contains(".env"), "log does not name the private path");

    // 6. Exactly two commits survive (base + docs update).
    let count = git(&out, &["rev-list", "--count", "main"]);
    assert_eq!(count, "2", "expected base + docs update only");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_embargoed_export_refuses_and_writes_nothing() {
    let tmp = std::env::temp_dir().join(format!("oot-embargo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    build_fixture(&src);
    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nembargo_until = \"2027-01-01\"\nprivate_branches = []\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, _) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok);

    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(!ok, "embargoed export must fail");
    assert!(
        msg.contains("embargo"),
        "error should mention embargo: {msg}"
    );
    assert!(
        !out.exists(),
        "embargoed export created the output directory"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Cached export mappings from one policy regime must never leak into
/// another: switching between filtered and unfiltered exports has to
/// invalidate the cache, or exports silently mix rewritten and original
/// objects into a franken-history.
#[test]
fn test_policy_change_invalidates_export_cache() {
    let tmp = std::env::temp_dir().join(format!("oot-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    build_fixture(&src);
    let env_blob = add_secret_and_followup(&src);
    let orig_head = git(&src, &["rev-parse", "main"]);

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    // 1. Unfiltered export reproduces the original head.
    let out_plain = tmp.join("out-plain");
    let (ok, msg) = oot(&["export", "--out", out_plain.to_str().unwrap()], &proj);
    assert!(ok, "unfiltered export failed: {msg}");
    assert_eq!(git(&out_plain, &["rev-parse", "main"]), orig_head);

    // 2. Filtered export must NOT reuse those cached shas.
    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nprivate_branches = []\n",
    )
    .unwrap();
    let out_filt = tmp.join("out-filt");
    let (ok, msg) = oot(&["export", "--out", out_filt.to_str().unwrap()], &proj);
    assert!(ok, "filtered export failed: {msg}");
    let filt_head = git(&out_filt, &["rev-parse", "main"]);
    assert_ne!(filt_head, orig_head);

    // 3. Re-exporting under the same policy is stable...
    let out_filt2 = tmp.join("out-filt2");
    let (ok, msg) = oot(&["export", "--out", out_filt2.to_str().unwrap()], &proj);
    assert!(ok, "second filtered export failed: {msg}");
    assert_eq!(git(&out_filt2, &["rev-parse", "main"]), filt_head);

    // 4. ...and switching back to unfiltered restores original shas.
    std::fs::remove_file(proj.join("visibility.toml")).unwrap();
    let out_plain2 = tmp.join("out-plain2");
    let (ok, msg) = oot(&["export", "--out", out_plain2.to_str().unwrap()], &proj);
    assert!(ok, "re-unfiltered export failed: {msg}");
    assert_eq!(git(&out_plain2, &["rev-parse", "main"]), orig_head);

    // The secret blob must be reachable in the plain exports only.
    for dir in [&out_plain, &out_plain2] {
        let check = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["cat-file", "-e", &env_blob])
            .output()
            .unwrap();
        assert!(check.status.success());
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_filtered_export_skips_empty_rebuilt_commits() {
    let tmp = std::env::temp_dir().join(format!("oot-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    build_fixture(&src);
    git(&src, &["commit", "--allow-empty", "-m", "empty churn"]);
    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\".env\"]\nprivate_branches = []\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    let count = git(&out, &["rev-list", "--count", "main"]);
    assert_eq!(count, "1", "empty rebuilt commit must be skipped");

    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        log.contains("empty after private-path stripping"),
        "log should record the empty skip: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_filtered_export_prevents_renamed_private_file_leak() {
    let tmp = std::env::temp_dir().join(format!("oot-rename-leak-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    build_fixture(&src);
    // Commit 2: introduce secret in secrets/pass.txt
    std::fs::create_dir_all(src.join("secrets")).unwrap();
    std::fs::write(src.join("secrets/pass.txt"), "supersecretpassword\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "add secret"]);

    // Commit 3: rename secrets/pass.txt to leaked_pass.txt
    git(&src, &["mv", "secrets/pass.txt", "leaked_pass.txt"]);
    git(&src, &["commit", "-m", "move secret outside private path"]);

    // Commit 4: modify public file
    std::fs::write(src.join("README.md"), "v2 with updates\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "public docs update"]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\"secrets/\"]\nprivate_branches = []\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    // Verify exported repository does not have leaked_pass.txt in working tree or HEAD commit
    assert!(
        !out.join("leaked_pass.txt").exists(),
        "leaked_pass.txt must not exist in working tree"
    );
    let ls = git(&out, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        !ls.contains("leaked_pass.txt"),
        "leaked_pass.txt must not exist in HEAD tree: {ls}"
    );
    assert!(
        !ls.contains("secrets/pass.txt"),
        "secrets/pass.txt must not exist in HEAD tree: {ls}"
    );
    assert!(
        ls.contains("README.md"),
        "README.md should exist in HEAD tree: {ls}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
