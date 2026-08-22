//! `oot record`: the first write path that belongs to Oot itself. A recorded
//! change must behave exactly like an imported one downstream — export turns
//! native history into plain git commits with real content.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_oot")
}

fn oot(args: &[&str], cwd: &Path) -> (bool, String) {
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
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .envs([
            ("GIT_AUTHOR_NAME", "K"),
            ("GIT_AUTHOR_EMAIL", "k@x.dev"),
            ("GIT_COMMITTER_NAME", "K"),
            ("GIT_COMMITTER_EMAIL", "k@x.dev"),
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

/// A project with no git history at all: Oot is the only VCS here.
fn seed_project(root: &Path) {
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    std::fs::write(root.join("sub/b.txt"), "deep\n").unwrap();
    std::fs::write(root.join(".gitignore"), "junk.log\n").unwrap();
    std::fs::write(root.join("junk.log"), "ignored junk\n").unwrap();
}

#[test]
fn test_record_creates_exportable_native_history() {
    let tmp = std::env::temp_dir().join(format!("oot-record-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    seed_project(&proj);
    assert!(oot(&["init"], &proj).0);

    let (ok, msg) = oot(&["record", "-m", "first change"], &proj);
    assert!(ok, "first record failed: {msg}");

    // Working copy unchanged -> nothing to record.
    let (ok, msg) = oot(&["record", "-m", "no-op"], &proj);
    assert!(!ok, "no-change record must fail");
    assert!(msg.contains("nothing to record"), "{msg}");

    // Edit a file and record again.
    std::fs::write(proj.join("a.txt"), "changed\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "second change"], &proj);
    assert!(ok, "second record failed: {msg}");

    // Exported as plain git: two commits, correct content and messages.
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    let log = git(&out, &["log", "--format=%s"]);
    assert_eq!(log.lines().count(), 2, "expected two commits");
    assert!(log.contains("first change") && log.contains("second change"));

    assert_eq!(git(&out, &["show", "main:a.txt"]), "changed");
    assert_eq!(git(&out, &["show", "main:sub/b.txt"]), "deep");

    // .gitignore was captured but ignored junk never entered history.
    let names = git(&out, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(names.contains(".gitignore"));
    assert!(!names.contains("junk.log"), "ignored file leaked: {names}");

    // The diff between the two records is exactly the a.txt edit.
    let diff = git(&out, &["diff", "main~1", "main", "--name-only"]);
    assert_eq!(diff, "a.txt");

    // Native changes carry authorship.
    let author = git(&out, &["log", "--format=%an <%ae>", "-n", "1"]);
    assert_eq!(author, "Kriday <k@oot.dev>");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Mixed history: imported git commits followed by native records must
/// export as one coherent linear history.
#[test]
fn test_record_extends_imported_history() {
    let tmp = std::env::temp_dir().join(format!("oot-record-mixed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&proj).unwrap();

    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("seed.txt"), "seed\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "imported root"]);
    std::fs::copy(src.join("seed.txt"), proj.join(".keep")).unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");

    std::fs::write(proj.join("native.txt"), "born in oot\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "native change"], &proj);
    assert!(ok, "record failed: {msg}");

    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    let log = git(&out, &["log", "--format=%s"]);
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec!["native change", "imported root"]
    );
    assert_eq!(git(&out, &["show", "main:native.txt"]), "born in oot");
    assert_eq!(git(&out, &["show", "main:.keep"]), "seed");

    let _ = std::fs::remove_dir_all(&tmp);
}
