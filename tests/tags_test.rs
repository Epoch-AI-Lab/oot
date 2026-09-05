//! Tags round-trip: git tags -> Oot store -> exported git repo.
//!
//! Import peels every tag (lightweight or annotated) to its target commit
//! and records the change id. Export writes lightweight `refs/tags/*` refs,
//! walking withheld history to the nearest kept ancestor like branches do.

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

/// Two-commit history with a lightweight tag, an annotated tag, and a
/// slashed tag name.
fn build_tagged_source(path: &PathBuf) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "-b", "main"]);
    std::fs::write(path.join("a.txt"), "v1\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "first"]);
    git(path, &["tag", "v0.1"]);
    std::fs::write(path.join("a.txt"), "v2\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "second"]);
    git(path, &["tag", "-a", "v0.2", "-m", "second release"]);
    git(path, &["tag", "release/rc1"]);
}

#[test]
fn test_tags_roundtrip_byte_identical() {
    let tmp = std::env::temp_dir().join(format!("oot-tags-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();
    build_tagged_source(&src);

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    assert!(msg.contains("tag v0.1"), "import should report tags: {msg}");

    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    // Every tag survived as a lightweight ref to a commit.
    for tag in ["v0.1", "v0.2", "release/rc1"] {
        let kind = git(&out, &["cat-file", "-t", &format!("refs/tags/{tag}")]);
        assert_eq!(kind, "commit", "exported {tag} should be lightweight");
        // Identity fast path: untouched history keeps original shas.
        let want = git(&src, &["rev-list", "-n", "1", tag]);
        let got = git(&out, &["rev-parse", &format!("refs/tags/{tag}")]);
        assert_eq!(got, want, "tag {tag} moved");
    }
}

/// Base commit, secret commit, clean follow-up. A tag on the withheld
/// secret commit falls back to the nearest kept ancestor (the base).
#[test]
fn test_tags_follow_withheld_history_to_kept_ancestor() {
    let tmp = std::env::temp_dir().join(format!("oot-tags-fb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "base"]);
    let base = git(&src, &["rev-parse", "main"]);
    std::fs::write(src.join(".env"), "API_KEY=x\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "secret"]);
    git(&src, &["tag", "on-secret"]);
    std::fs::write(src.join("README.md"), "v2\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "followup"]);

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

    let got = git(&out, &["rev-parse", "refs/tags/on-secret"]);
    assert_eq!(got, base, "withheld tag should fall back to base commit");
}

/// A tag whose entire history was withheld is omitted with a log entry.
#[test]
fn test_tags_omitted_when_all_history_withheld() {
    let tmp = std::env::temp_dir().join(format!("oot-tags-om-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join(".env"), "API_KEY=x\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "only secret"]);
    git(&src, &["tag", "doomed"]);

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
    assert!(
        msg.contains("omitted"),
        "export should report omission: {msg}"
    );

    let refs = git(&out, &["for-each-ref", "--format=%(refname)", "refs/tags"]);
    assert!(refs.is_empty(), "withheld tag leaked: {refs}");
    let probe = Command::new("git")
        .arg("-C")
        .arg(&out)
        .args(["rev-parse", "--verify", "--quiet", "refs/tags/doomed"])
        .output()
        .unwrap();
    assert!(!probe.status.success(), "withheld tag resolves");

    let log = std::fs::read_to_string(proj.join(".oot").join("export-log.jsonl")).unwrap();
    assert!(
        log.contains("tag-omitted") && log.contains("doomed"),
        "omission must be audited: {log}"
    );
}

/// A tag pointing at a blob (not a commit) is skipped with a warning, the
/// rest of the import succeeds, and re-importing is idempotent.
#[test]
fn test_tags_skip_non_commit_targets_and_reimport() {
    let tmp = std::env::temp_dir().join(format!("oot-tags-sk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("a.txt"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "first"]);
    git(&src, &["tag", "good"]);
    let blob = git(&src, &["hash-object", "-w", "a.txt"]);
    git(&src, &["tag", "notacommit", &blob]);

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    assert!(
        msg.contains("tag good: imported"),
        "good tag missing: {msg}"
    );
    assert!(
        msg.contains("notacommit") && msg.contains("skipped"),
        "blob tag should skip with a warning: {msg}"
    );

    // Second import over the same store succeeds and changes nothing.
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "reimport failed: {msg}");

    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");
    let refs = git(
        &out,
        &["for-each-ref", "--format=%(refname:short)", "refs/tags"],
    );
    assert_eq!(refs, "good", "only the commit tag should export: {refs}");
}
