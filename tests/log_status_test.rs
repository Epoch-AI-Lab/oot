//! `oot status` and `oot log`: the read side of Oot's native write path.
//! Status must tell the truth about what a record would capture; log must
//! show native and imported changes in one coherent newest-first list.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_oot")
}

fn oot(args: &[&str], cwd: &Path) -> (bool, String) {
    let o = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Kriday")
        .env("GIT_AUTHOR_EMAIL", "k@oot.dev")
        .env("GIT_COMMITTER_NAME", "Kriday")
        .env("GIT_COMMITTER_EMAIL", "k@oot.dev")
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

fn git(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
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
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn test_status_reports_deltas_then_clean_after_record() {
    let tmp = std::env::temp_dir().join(format!("oot-status-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    std::fs::write(proj.join("gone.txt"), "bye\n").unwrap();
    assert!(oot(&["init"], &proj).0);

    // Empty store: everything is a would-be capture.
    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("no changes yet"), "{out}");
    assert!(
        out.contains("A a.txt") && out.contains("A gone.txt"),
        "{out}"
    );

    // Record one file away to build a head, then re-add it so the delta vs
    // head is: a.txt added, gone.txt deleted.
    std::fs::write(proj.join("base.txt"), "base\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "base"], &proj);
    assert!(ok, "{msg}");
    std::fs::remove_file(proj.join("base.txt")).unwrap();
    std::fs::write(proj.join("a.txt"), "v2\n").unwrap();

    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("M a.txt"), "{out}");
    assert!(out.contains("D base.txt"), "{out}");
    assert!(!out.contains("A "), "{out}");

    let (ok, msg) = oot(&["record", "-m", "delta"], &proj);
    assert!(ok, "{msg}");

    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Log walks imported and recorded changes together, newest first, tagging
/// where each change came from.
#[test]
fn test_log_lists_native_and_imported_newest_first() {
    let tmp = std::env::temp_dir().join(format!("oot-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    std::fs::create_dir_all(&src).unwrap();
    assert!(git(&src, &["init", "--quiet", "-b", "main"]));
    std::fs::write(src.join("s.txt"), "seed\n").unwrap();
    assert!(git(&src, &["add", "."]));
    assert!(git(&src, &["commit", "-m", "imported root"]));

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "{msg}");

    std::fs::write(proj.join("native.txt"), "n\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first native"], &proj);
    assert!(ok, "{msg}");
    std::fs::write(proj.join("native.txt"), "m\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "second native"], &proj);
    assert!(ok, "{msg}");

    let (ok, out) = oot(&["log"], &proj);
    assert!(ok, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(
        !lines[0].starts_with(' ') && lines[0].contains("second native [oot]"),
        "{out}"
    );
    assert!(lines[1].contains("first native [oot]"), "{out}");
    assert!(lines[2].contains("imported root [git]"), "{out}");
    // Author of the native change comes through.
    assert!(lines[0].contains("Kriday"), "{out}");

    // Log on an unknown/empty branch fails loudly rather than printing junk.
    let (ok, out) = oot(&["log", "--branch", "ghost"], &proj);
    assert!(!ok, "log on empty branch must fail");
    assert!(out.contains("ghost"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}
