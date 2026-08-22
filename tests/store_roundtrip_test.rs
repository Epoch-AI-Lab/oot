//! End-to-end round-trip: git history -> Oot store -> exported git repo.
//!
//! The contract is byte-exactness: because the store preserves trees,
//! authorship, timestamps, offsets, messages, and parent structure, git's
//! content addressing must reproduce every original commit SHA on export.
//! If a SHA differs, something was lost or rewritten.

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

/// Build a history that stresses every fidelity axis we claim to preserve:
/// merges, non-UTC timezone offsets, binary content, unicode + multi-paragraph
/// messages, and two branches sharing a base.
fn build_source_repo(path: &PathBuf) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "-b", "main"]);
    git(
        path,
        &[
            "-c",
            "user.email=k@oot.dev",
            "-c",
            "user.name=Kriday",
            "commit",
            "--allow-empty",
            "-m",
            "root commit",
        ],
    );

    // Non-UTC offset on an early commit.
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "commit",
            "--allow-empty",
            "-m",
            "auth flow\n\nDetailed body.\n",
        ])
        .env("GIT_AUTHOR_DATE", "1700000000 +0530")
        .env("GIT_COMMITTER_DATE", "1700000001 -0800")
        .output()
        .unwrap();
    assert!(out.status.success());

    // Binary file (NUL bytes) plus unicode message.
    std::fs::write(path.join("logo.bin"), vec![0u8, 159, 146, 150, 255]).unwrap();
    git(path, &["add", "."]);
    git(
        path,
        &["commit", "-m", "binaire ✓ ünïcode\n\nBody with — dashes.\n"],
    );

    // Branch off, then merge with --no-ff so a real merge commit exists.
    git(path, &["checkout", "-q", "-b", "feature/greet"]);
    std::fs::write(path.join("greet.txt"), "hello").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "greet"]);

    git(path, &["checkout", "-q", "main"]);
    std::fs::write(path.join("main.txt"), "m").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "main work"]);

    git(
        path,
        &[
            "-c",
            "user.email=k@oot.dev",
            "-c",
            "user.name=Kriday",
            "merge",
            "--no-ff",
            "-m",
            "Merge branch 'feature/greet'",
            "feature/greet",
        ],
    );
}

/// A commit carrying an extra header (the shape GitHub's signed merges have)
/// cannot be rebuilt by `commit-tree`; export must reuse the original object
/// so the sha — and the header itself — survive byte-exact.
#[test]
fn test_roundtrip_preserves_extra_commit_headers() {
    let tmp = std::env::temp_dir().join(format!("oot-signed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    build_source_repo(&src);
    let root = git(&src, &["rev-parse", "main"]);
    let tree = git(&src, &["rev-parse", &format!("{root}^{{tree}}")]);

    // Hand-forge a commit on top of main with a gpgsig header, exactly how a
    // real signature sits in the object: continuation lines start with space.
    let raw = format!(
        "tree {tree}\nparent {root}\n\
         author Kriday <k@oot.dev> 1700000100 +0530\n\
         committer Kriday <k@oot.dev> 1700000101 +0530\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n \
         iQEcBAABCgAGBQJlxyz0AAoJEDummy\n \
         =abcd\n \
         -----END PGP SIGNATURE-----\n\
         \n\
         signed commit\n"
    );
    let raw_path = src.join("forged.commit");
    std::fs::write(&raw_path, &raw).unwrap();
    let forged = Command::new("git")
        .arg("-C")
        .arg(&src)
        .args(["hash-object", "-t", "commit", "-w", "forged.commit"])
        .output()
        .unwrap();
    assert!(forged.status.success());
    let signed_sha = String::from_utf8_lossy(&forged.stdout).trim().to_string();
    std::fs::remove_file(&raw_path).unwrap();
    git(&src, &["update-ref", "refs/heads/main", &signed_sha]);

    let run = |args: &[&str], cwd: &Path| {
        let o = Command::new(bin())
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("oot binary should run");
        assert!(
            o.status.success(),
            "oot {:?} failed: {}\n{}",
            args,
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };

    run(&["init"], &proj);
    run(&["import", "--repo", src.to_str().unwrap()], &proj);
    run(&["export", "--out", out.to_str().unwrap()], &proj);

    let exported = git(&out, &["rev-parse", "main"]);
    assert_eq!(exported, signed_sha, "signed commit diverged after export");

    // The header bytes themselves must be present in the exported object.
    let body = Command::new("git")
        .arg("-C")
        .arg(&out)
        .args(["cat-file", "commit", "main"])
        .output()
        .unwrap();
    let body = String::from_utf8_lossy(&body.stdout).to_string();
    assert!(body.contains("BEGIN PGP SIGNATURE"), "signature lost");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_full_roundtrip_preserves_every_commit_sha() {
    let tmp = std::env::temp_dir().join(format!("oot-roundtrip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    build_source_repo(&src);
    let all_shas: Vec<String> = git(&src, &["rev-list", "--all"])
        .lines()
        .map(str::to_string)
        .collect();
    assert!(all_shas.len() >= 6, "fixture should have enough commits");

    let run = |args: &[&str], cwd: &Path| {
        let o = Command::new(bin())
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("oot binary should run");
        assert!(
            o.status.success(),
            "oot {:?} failed: {}\n{}",
            args,
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };

    run(&["init"], &proj);
    let import_out = run(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(import_out.contains("branch main"));
    assert!(import_out.contains("branch feature/greet"));

    // Import is idempotent: a second pass must not duplicate anything.
    run(&["import", "--repo", src.to_str().unwrap()], &proj);

    run(&["export", "--out", out.to_str().unwrap()], &proj);

    // 1. Every original commit SHA must exist byte-exact in the export.
    for sha in &all_shas {
        let check = Command::new("git")
            .arg("-C")
            .arg(&out)
            .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "exported repo is missing original commit {sha}"
        );
    }

    // 2. Branch refs must point at exactly the original SHAs.
    for branch in ["main", "feature/greet"] {
        let orig = git(&src, &["rev-parse", branch]);
        let exported = git(&out, &["rev-parse", branch]);
        assert_eq!(orig, exported, "ref {branch} diverged after round-trip");
    }

    // 3. Trees must match per commit (content fidelity, not just DAG shape).
    for sha in &all_shas {
        let orig_tree = git(&src, &["rev-parse", &format!("{sha}^{{tree}}")]);
        let exp_tree = git(&out, &["rev-parse", &format!("{sha}^{{tree}}")]);
        assert_eq!(orig_tree, exp_tree, "tree of {sha} diverged");
    }

    // 4. Commit metadata survived: timezone offset, body, parents.
    let orig_meta = git(
        &src,
        &[
            "log",
            "--format=%an|%ae|%ai|%P|%B",
            "--max-count=2",
            "main~2",
        ],
    );
    let exp_meta = git(
        &out,
        &[
            "log",
            "--format=%an|%ae|%ai|%P|%B",
            "--max-count=2",
            "main~2",
        ],
    );
    assert_eq!(orig_meta, exp_meta, "author/date/parent metadata diverged");

    // 5. Export is idempotent at the store level: re-exporting into a fresh
    //    directory yields the same SHAs from cached mappings.
    let out2 = tmp.join("out2");
    run(&["export", "--out", out2.to_str().unwrap()], &proj);
    let orig = git(&src, &["rev-parse", "main"]);
    let again = git(&out2, &["rev-parse", "main"]);
    assert_eq!(orig, again);

    let _ = std::fs::remove_dir_all(&tmp);
}
