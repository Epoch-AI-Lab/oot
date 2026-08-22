//! Store-to-court wiring: `oot adjudicate --change` reads history straight
//! from `.oot/`, judges a change against its FIRST parent (root changes diff
//! against nothing), and persists the verdict as sidecar dockets plus an
//! append-only audit trail. Exit codes stay the court's: 0 only Adjudicated.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_oot")
}

struct Run {
    code: Option<i32>,
    out: String,
}

fn oot(args: &[&str], cwd: &Path) -> Run {
    let o = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        // Identity via env: CI runners have no global git config.
        .env("GIT_AUTHOR_NAME", "Kriday")
        .env("GIT_AUTHOR_EMAIL", "k@oot.dev")
        .env("GIT_COMMITTER_NAME", "Kriday")
        .env("GIT_COMMITTER_EMAIL", "k@oot.dev")
        .output()
        .expect("oot binary should run");
    Run {
        code: o.status.code(),
        out: format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    }
}

fn ok(run: &Run, what: &str) {
    assert_eq!(run.code, Some(0), "{what} failed: {}", run.out);
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .envs([
            ("GIT_AUTHOR_NAME", "Git K"),
            ("GIT_AUTHOR_EMAIL", "g@x.dev"),
            ("GIT_COMMITTER_NAME", "Git K"),
            ("GIT_COMMITTER_EMAIL", "g@x.dev"),
        ])
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

fn fresh(tag: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("oot-court-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    proj
}

/// Record the working copy and return the full change id.
fn record(proj: &Path, msg: &str) -> String {
    let run = oot(&["record", "-m", msg], proj);
    ok(&run, "record");
    run.out
        .lines()
        .find(|l| l.starts_with("recorded "))
        .unwrap_or_else(|| panic!("no record line in: {}", run.out))
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string()
}

/// Change ids of a branch, oldest first, parsed from `oot log` output.
fn logged_ids(proj: &Path) -> Vec<String> {
    let run = oot(&["log"], proj);
    ok(&run, "log");
    let mut ids: Vec<String> = run
        .out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect();
    ids.reverse();
    ids
}

fn audit_lines(proj: &Path) -> Vec<String> {
    std::fs::read_to_string(proj.join(".oot/adjudications.jsonl"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn test_root_change_adjudicates_clean_exit_zero() {
    let proj = fresh("happy");

    std::fs::write(
        proj.join("lib.rs"),
        "pub fn greet() -> &'static str { \"hi\" }\n",
    )
    .unwrap();
    ok(&oot(&["init"], &proj), "init");
    let root = record(&proj, "root change");

    // Prefixes resolve like full ids.
    let run = oot(&["adjudicate", "--change", &root[..7]], &proj);
    assert_eq!(run.code, Some(0), "{}", run.out);
    assert!(run.out.contains("ADJUDICATED"), "{}", run.out);
    assert!(run.out.contains("from:       oot"), "{}", run.out);
    assert!(run.out.contains("base:       (root)"), "{}", run.out);

    // The docket landed in the sidecar under the full id.
    assert!(proj
        .join(".oot/dockets")
        .join(format!("{root}.json"))
        .exists());
}

#[test]
fn test_meaning_dispute_blocks_under_strict_policy_exit_one() {
    let proj = fresh("blocked");

    std::fs::write(proj.join("lib.rs"), "pub fn calc() -> i32 { 1 }\n").unwrap();
    ok(&oot(&["init"], &proj), "init");
    record(&proj, "base");
    std::fs::write(proj.join("lib.rs"), "pub fn calc() -> i32 { 2 }\n").unwrap();
    let child = record(&proj, "edit calc");

    // Default policy: a changed function is Review-level, not blocking.
    let run = oot(&["adjudicate", "--change", &child], &proj);
    assert_eq!(run.code, Some(0), "{}", run.out);

    // Strict policy: review blocks. Exit contract unchanged — nonzero.
    std::fs::write(
        proj.join("strict.toml"),
        "block_on = [\"review\"]\nreview_on = []\n",
    )
    .unwrap();
    let run = oot(
        &[
            "adjudicate",
            "--change",
            &child,
            "--policy",
            "strict.toml",
            "--no-save",
        ],
        &proj,
    );
    assert_eq!(run.code, Some(1), "{}", run.out);
    assert!(run.out.contains("BLOCKED"), "{}", run.out);
    assert!(run.out.contains("both sides changed `calc`"), "{}", run.out);
}

#[test]
fn test_child_change_shows_delta_not_whole_tree_noise() {
    let proj = fresh("delta");

    std::fs::write(proj.join("lib.rs"), "pub fn calc() -> i32 { 1 }\n").unwrap();
    std::fs::write(proj.join("keep.txt"), "untouched\n").unwrap();
    ok(&oot(&["init"], &proj), "init");
    record(&proj, "base with two files");

    // Only lib.rs moves; keep.txt stays identical between parent and child.
    std::fs::write(proj.join("lib.rs"), "pub fn calc() -> i32 { 2 }\n").unwrap();
    let child = record(&proj, "edit calc only");

    let run = oot(&["adjudicate", "--change", &child], &proj);
    assert_eq!(run.code, Some(0), "{}", run.out);
    assert!(run.out.contains("`calc`"), "{}", run.out);
    assert!(run.out.contains("intent:     lib.rs"), "{}", run.out);
    assert!(
        !run.out.contains("keep.txt"),
        "untouched file leaked into the docket: {}",
        run.out
    );
}

#[test]
fn test_persistence_overwrite_and_audit_line_per_run() {
    let proj = fresh("persist");

    std::fs::write(proj.join("notes.txt"), "v1\n").unwrap();
    ok(&oot(&["init"], &proj), "init");
    let id = record(&proj, "only change");
    let short = id[..7].to_string();

    ok(
        &oot(&["adjudicate", "--change", &short], &proj),
        "first adjudication",
    );
    let docket_path = proj.join(".oot/dockets").join(format!("{id}.json"));
    let first = std::fs::read_to_string(&docket_path).unwrap();
    assert!(first.contains("\"schema\": 1"), "{first}");
    assert_eq!(audit_lines(&proj).len(), 1);
    assert!(audit_lines(&proj)[0].contains("\"event\":\"adjudicated\""));
    assert!(audit_lines(&proj)[0].contains(&format!("\"change\":\"{id}\"")));

    // Second run overwrites the sidecar but appends exactly one audit line.
    std::thread::sleep(std::time::Duration::from_secs(1));
    ok(
        &oot(&["adjudicate", "--change", &short], &proj),
        "second adjudication",
    );
    let second = std::fs::read_to_string(&docket_path).unwrap();
    assert_ne!(first, second, "re-run must refresh the envelope timestamp");
    assert_eq!(second.lines().count(), first.lines().count());
    let lines = audit_lines(&proj);
    assert_eq!(lines.len(), 2, "one jsonl line per run: {:?}", lines);

    // `--no-save` touches neither sidecar.
    ok(
        &oot(&["adjudicate", "--change", &short, "--no-save"], &proj),
        "no-save adjudication",
    );
    assert_eq!(audit_lines(&proj).len(), 2);

    // The persisted docket renders on demand.
    let run = oot(&["docket", &short], &proj);
    ok(&run, "oot docket");
    assert!(run.out.contains("OOT DOCKET"), "{}", run.out);
    assert!(run.out.contains(&short), "{}", run.out);
}

#[test]
fn test_imported_mid_history_change_carries_git_tag() {
    let proj = fresh("imported");
    let src = proj.parent().unwrap().join("src");
    std::fs::create_dir_all(&src).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("lib.rs"), "pub fn f() -> i32 { 1 }\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "imported root"]);
    std::fs::write(src.join("lib.rs"), "pub fn f() -> i32 { 2 }\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "mid-history edit"]);

    ok(&oot(&["init"], &proj), "init");
    ok(
        &oot(&["import", "--repo", src.to_str().unwrap()], &proj),
        "import",
    );

    // log prints newest first; flip to oldest-first and take the child.
    let ids = logged_ids(&proj);
    assert_eq!(ids.len(), 2);
    let mid = &ids[1];

    let run = oot(&["adjudicate", "--change", mid], &proj);
    assert_eq!(run.code, Some(0), "{}", run.out);
    assert!(run.out.contains("from:       git"), "{}", run.out);
    // Base is the imported parent, not "(root)".
    assert!(
        run.out.contains(&format!("base:       {}", ids[0])),
        "{}",
        run.out
    );
    assert!(run.out.contains("both sides changed `f`"), "{}", run.out);
}

#[test]
fn test_mixed_imported_and_native_history_adjudicates() {
    let proj = fresh("mixed");
    let src = proj.parent().unwrap().join("src");
    std::fs::create_dir_all(&src).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("seed.txt"), "seed\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "imported root"]);

    ok(&oot(&["init"], &proj), "init");
    ok(
        &oot(&["import", "--repo", src.to_str().unwrap()], &proj),
        "import",
    );

    // Materialize the imported head in the worktree so the only real delta
    // of the next record is the new file.
    std::fs::write(proj.join("seed.txt"), "seed\n").unwrap();
    std::fs::write(proj.join("native.txt"), "born in oot\n").unwrap();
    let native = record(&proj, "native child");

    let imported_root = &logged_ids(&proj)[0];
    let run = oot(&["adjudicate", "--change", imported_root], &proj);
    assert_eq!(run.code, Some(0), "{}", run.out);
    assert!(run.out.contains("from:       git"), "{}", run.out);

    let run = oot(&["adjudicate", "--change", &native], &proj);
    assert_eq!(run.code, Some(0), "{}", run.out);
    assert!(run.out.contains("from:       oot"), "{}", run.out);
    assert!(run.out.contains("intent:     native.txt"), "{}", run.out);
    assert!(!run.out.contains("seed.txt"), "{}", run.out);

    // Both dockets persist side by side (files are named by full change id;
    // log only ever surfaces short prefixes).
    let persisted = std::fs::read_dir(proj.join(".oot/dockets"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(persisted.len(), 2);
    assert_eq!(audit_lines(&proj).len(), 2);
}

#[test]
fn test_unknown_id_and_ambiguous_prefix_fail_loudly() {
    let proj = fresh("resolve");
    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    ok(&oot(&["init"], &proj), "init");
    record(&proj, "one real change");

    // Unknown id: loud error, exit 1, no usage fallback.
    let run = oot(&["adjudicate", "--change", "deadbeef"], &proj);
    assert_eq!(run.code, Some(1), "{}", run.out);
    assert!(
        run.out.contains("no change matching 'deadbeef'"),
        "{}",
        run.out
    );

    // Craft two stored changes sharing a long prefix; resolve_change works
    // on stored filenames so this is a deterministic ambiguity fixture.
    use oot::store::{ChangeRecord, Identity};
    let changes = proj.join(".oot/changes");
    for tail in ["1", "2"] {
        let id = format!("aaaa00000000000000000000000000000000000{tail}");
        let rec = ChangeRecord {
            parents: vec![],
            tree: format!("tree-{tail}"),
            author: Identity {
                name: "A".into(),
                email: "a@b.c".into(),
                time: 0,
                offset: "+0000".into(),
            },
            committer: Identity {
                name: "A".into(),
                email: "a@b.c".into(),
                time: 0,
                offset: "+0000".into(),
            },
            message: "crafted\n".into(),
            source_sha: None,
        };
        std::fs::write(
            changes.join(format!("{id}.json")),
            serde_json::to_vec(&rec).unwrap(),
        )
        .unwrap();
    }

    let run = oot(&["adjudicate", "--change", "aaaa"], &proj);
    assert_eq!(run.code, Some(1), "{}", run.out);
    assert!(
        run.out.contains("ambiguous change prefix 'aaaa'"),
        "{}",
        run.out
    );
    assert!(
        run.out.contains("aaaa000000000000000000000000000000000001"),
        "{}",
        run.out
    );
    assert!(
        run.out.contains("aaaa000000000000000000000000000000000002"),
        "{}",
        run.out
    );

    // `oot docket` resolves through the same rules.
    let run = oot(&["docket", "aaaa"], &proj);
    assert_eq!(run.code, Some(1), "{}", run.out);
    assert!(run.out.contains("ambiguous"), "{}", run.out);

    // Store mode does NOT engage when another mode is requested: with
    // --source given, --change falls back to the legacy snapshot modes,
    // which exit 2 on missing --base/--head even though a store exists.
    let run = oot(
        &["adjudicate", "--change", "aaaa", "--source", "memory"],
        &proj,
    );
    assert_eq!(run.code, Some(2), "{}", run.out);
    assert!(
        !run.out.contains("ambiguous"),
        "--source must disable store mode: {}",
        run.out
    );
}
