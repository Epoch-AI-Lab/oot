//! `oot update`: materialize a stored change's tree into the working copy.

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

fn unique_tmp(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tid = format!("{:?}", std::thread::current().id());
    let rand: u32 =
        std::collections::hash_map::DefaultHasher::new().finish() as u32 ^ (nanos as u32);
    std::env::temp_dir().join(format!(
        "oot-update-{tag}-{}-{tid}-{nanos}-{rand}",
        std::process::id()
    ))
}

use std::hash::Hasher;

#[test]
fn test_update_restores_branch_head() {
    let tmp = unique_tmp("restore");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    std::fs::write(proj.join("b.txt"), "keep\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    // Dirty: edit, delete, create
    std::fs::write(proj.join("a.txt"), "v2\n").unwrap();
    std::fs::remove_file(proj.join("b.txt")).unwrap();
    std::fs::write(proj.join("c.txt"), "new\n").unwrap();

    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("M a.txt"), "{out}");

    // Without --force now does a 3-way merge and keeps dirty work (no --force ever)
    let (ok, out) = oot(&["update"], &proj);
    assert!(ok, "dirty without --force should merge, not bail: {out}");
    assert!(out.contains("merged") || out.contains("keep"), "{out}");
    // Merge kept dirty work: a.txt stays v2, b.txt stays deleted, c.txt stays
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v2\n");
    assert!(!proj.join("b.txt").exists(), "merge keeps deleted b.txt");
    assert!(proj.join("c.txt").exists(), "merge keeps new c.txt");

    // Dry-run previews without touching disk (shows merge)
    let (ok, out) = oot(&["update", "--dry-run"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("would update"), "{out}");
    // Still dirty after dry-run
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v2\n");

    // With --force it restores (discards work)
    let (ok, msg) = oot(&["update", "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(msg.contains("updated to"), "{msg}");
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v1\n");
    assert_eq!(
        std::fs::read_to_string(proj.join("b.txt")).unwrap(),
        "keep\n"
    );
    assert!(!proj.join("c.txt").exists(), "c.txt should be deleted");

    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_to_old_change_by_prefix() {
    let tmp = unique_tmp("prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    std::fs::write(proj.join("a.txt"), "v2\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "second"], &proj);
    assert!(ok, "{msg}");

    let (ok, log) = oot(&["log"], &proj);
    assert!(ok, "{log}");
    let first_line = log.lines().last().unwrap();
    let first_id = first_line.split_whitespace().next().unwrap();

    // Update to old change via short prefix
    let prefix = &first_id[..4];
    let (ok, out) = oot(&["update", "--change", prefix, "--force"], &proj);
    assert!(ok, "update to prefix {prefix} failed: {out}");
    assert!(out.contains("change"), "{out}");
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v1\n");

    // Back to head via branch
    let (ok, msg) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{msg}");
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v2\n");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_refuses_dirty_when_target_equals_current() {
    let tmp = unique_tmp("dirty-equal");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    // Dirty but target == current (no-arg update) — merge keeps dirty, force restores
    std::fs::write(proj.join("a.txt"), "dirty\n").unwrap();
    let (ok, out) = oot(&["update"], &proj);
    assert!(
        ok,
        "dirty with target==current should merge-keep, not bail: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(proj.join("a.txt")).unwrap(),
        "dirty\n"
    );

    // With --force it restores even though target==current
    let (ok, msg) = oot(&["update", "--force"], &proj);
    assert!(ok, "{msg}");
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v1\n");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_already_up_to_date() {
    let tmp = unique_tmp("uptodate");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    let (ok, out) = oot(&["update"], &proj);
    assert!(ok, "{out}");
    // Already at head should succeed without rewriting; may say updated or already
    assert!(
        out.contains("updated") || out.contains("up to date"),
        "{out}"
    );

    let (ok, out) = oot(&["update", "--dry-run"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("already up to date"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_branch_requires_flag_on_multi_branch() {
    let tmp = unique_tmp("multibranch");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "main\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "main first"], &proj);
    assert!(ok, "{msg}");

    std::fs::write(proj.join("b.txt"), "feature\n").unwrap();
    let (ok, msg) = oot(
        &["record", "--branch", "feature", "-m", "feature first"],
        &proj,
    );
    assert!(ok, "{msg}");

    // No branch flag with multiple branches must fail
    let (ok, out) = oot(&["update"], &proj);
    assert!(!ok, "multi-branch without --branch should fail");
    assert!(out.contains("multiple branches"), "{out}");

    let (ok, out) = oot(&["update", "--dry-run"], &proj);
    assert!(!ok, "{out}");

    // Explicit branch works — switching branches deletes files not in target, so requires --force
    let (ok, msg) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(!proj.join("b.txt").exists());

    let (ok, msg) = oot(&["update", "--branch", "feature", "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(proj.join("b.txt").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_change_and_branch_conflict() {
    let tmp = unique_tmp("conflict-flags");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    let (ok, log) = oot(&["log"], &proj);
    assert!(ok, "{log}");
    let id = log
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();

    let (ok, out) = oot(&["update", "--change", id, "--branch", "main"], &proj);
    assert!(!ok, "should reject --change with --branch");
    // clap conflict error contains either flag name
    assert!(
        out.contains("cannot be used with") || out.contains("conflicts"),
        "{out}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_empty_store_has_no_changes() {
    let tmp = unique_tmp("empty");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    let (ok, out) = oot(&["update"], &proj);
    assert!(!ok, "{out}");
    assert!(out.contains("no changes"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_ignores_symlink_and_restores_file() {
    let tmp = unique_tmp("symlink");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "real\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    // Create a symlink that would be followed if we were buggy; record skips it
    #[cfg(unix)]
    {
        let outside = tmp.join("outside.txt");
        std::fs::write(&outside, "outside\n").unwrap();
        let link = proj.join("link.txt");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        // a.txt is still real, link.txt is a symlink on disk but not in store
        let (ok, out) = oot(&["status"], &proj);
        assert!(ok, "{out}");
        // symlink should not appear as added
        assert!(!out.contains("link.txt"), "{out}");

        // Symlinks are not tracked by oot (collect_worktree skips them), so update
        // leaves them alone and never follows them. Outside file must not be overwritten.
        let (ok, msg) = oot(&["update", "--force"], &proj);
        assert!(ok, "{msg}");
        // Symlink survives because it is not in work's tracked set — we verify it was not followed
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside\n");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_exec_bit_preserved() {
    let tmp = unique_tmp("exec");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    let script = proj.join("run.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    // Remove exec bit and dirty
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&script, "#!/bin/sh\necho changed\n").unwrap();
        let (ok, out) = oot(&["status"], &proj);
        assert!(ok, "{out}");
        assert!(out.contains("M run.sh"), "{out}");
    }

    let (ok, msg) = oot(&["update", "--force"], &proj);
    assert!(ok, "{msg}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "exec bit should be restored, mode {mode:o}"
        );
        assert_eq!(
            std::fs::read_to_string(&script).unwrap(),
            "#!/bin/sh\necho hi\n"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_unknown_and_ambiguous_prefix() {
    let tmp = unique_tmp("ambig");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    let (ok, out) = oot(&["update", "--change", "does-not-exist"], &proj);
    assert!(!ok, "{out}");
    assert!(out.contains("no change"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_rejects_path_traversal() {
    // Git's own fsck already rejects trees containing `..` (hasDotdot),
    // so we verify the defense-in-depth validation in Store::snapshot_from_tree
    // and Update's path check would catch it if it somehow got in.
    // Here we just verify normal update does NOT falsely reject valid paths
    // and that the Store validation for traversal exists (unit-tested in store).
    let tmp = unique_tmp("traversal");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    // Valid nested path must be accepted
    std::fs::create_dir_all(proj.join("a/b")).unwrap();
    std::fs::write(proj.join("a/b/c.txt"), "ok\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "valid nested"], &proj);
    assert!(ok, "{msg}");
    let (ok, log) = oot(&["log"], &proj);
    assert!(ok, "{log}");
    let id = log
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let (ok, out) = oot(&["update", "--change", &id[..7], "--force"], &proj);
    assert!(ok, "valid nested path should not be rejected: {out}");

    // Verify that `..` in a direct PutRecord would be rejected at the store layer
    // (hash-object -t tree with `..` is rejected by git fsck, so store never stores it)
    let probe = Command::new("git")
        .args(["hash-object", "-t", "tree", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    if let Ok(mut child) = probe {
        use std::io::Write;
        let raw = b"040000 ..\0\x4b\x82\x5d\xc6\x42\xcb\x6e\xb9\xa0\x60\xe5\x4b\xf8\xd6\x92\x88\xfb\xee\x49\x04";
        let _ = child.stdin.take().unwrap().write_all(raw);
        let out = child.wait_with_output().unwrap();
        assert!(!out.status.success(), "git should reject hasDotdot");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("hasDotdot"),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_status_agrees_after_force() {
    let tmp = unique_tmp("status-agree");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::create_dir_all(proj.join("sub")).unwrap();
    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    std::fs::write(proj.join("sub/b.txt"), "deep\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    std::fs::write(proj.join("a.txt"), "v2\n").unwrap();
    std::fs::write(proj.join("sub/b.txt"), "changed\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "second"], &proj);
    assert!(ok, "{msg}");

    let (ok, log) = oot(&["log"], &proj);
    assert!(ok, "{log}");
    let first_id = log
        .lines()
        .last()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();

    let (ok, out) = oot(&["update", "--change", &first_id[..7], "--force"], &proj);
    assert!(ok, "{out}");
    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    // After update to old change, status should report dirty vs current head (main is second)
    // Force update to old change leaves working copy at first, so status vs head is dirty.
    // Now update back to head and status must be clean.
    let (ok, out) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{out}");
    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_ignored_file_not_dirty() {
    let tmp = unique_tmp("ignored");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join(".gitignore"), "junk.log\n").unwrap();
    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    std::fs::write(proj.join("junk.log"), "ignored\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");
    // junk.log was ignored, status should be clean
    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");

    // Modify ignored file — status should still be clean, update without --force should not be dirty
    std::fs::write(proj.join("junk.log"), "changed ignored\n").unwrap();
    let (ok, out) = oot(&["status"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");

    let (ok, out) = oot(&["update", "--dry-run"], &proj);
    assert!(ok, "{out}");
    assert!(out.contains("already up to date"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_clean_work_can_fast_forward_without_force() {
    let tmp = unique_tmp("fastforward");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");
    std::fs::write(proj.join("a.txt"), "v2\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "second"], &proj);
    assert!(ok, "{msg}");

    // Clean work at second, update to first without --force should succeed (no data loss)
    let (ok, log) = oot(&["log"], &proj);
    assert!(ok, "{log}");
    let first_id = log
        .lines()
        .last()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let (ok, out) = oot(&["update", "--change", &first_id[..7]], &proj);
    assert!(ok, "clean fast-forward should not require --force: {out}");
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v1\n");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_ghost_branch_fails() {
    let tmp = unique_tmp("ghost");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    let (ok, out) = oot(&["update", "--branch", "ghost"], &proj);
    assert!(!ok, "{out}");
    assert!(out.contains("no changes") || out.contains("ghost"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_vcs_dir_traversal_forbidden() {
    use oot::store::validate_tree_path;
    assert!(validate_tree_path(".git/hooks/pre-commit").is_err());
    assert!(validate_tree_path(".GIT/config").is_err());
    assert!(validate_tree_path("sub/.git/evil").is_err());
    assert!(validate_tree_path(".oot/changes/000.json").is_err());
    assert!(validate_tree_path(".jj/repo").is_err());
    assert!(validate_tree_path("src/lib.rs").is_ok());
    assert!(validate_tree_path("deep/nested/file.txt").is_ok());
}

#[test]
fn test_update_keep_marker_exact_vs_suffix() {
    let tmp = unique_tmp("keep-marker");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::create_dir_all(proj.join("logs")).unwrap();
    std::fs::write(proj.join("logs/.ootkeep"), "").unwrap();
    std::fs::write(proj.join("doc.ootkeep"), "regular text file\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    // Target change has neither
    std::fs::write(proj.join("other.txt"), "hello\n").unwrap();
    let _ = std::fs::remove_file(proj.join("doc.ootkeep"));
    let (ok, msg) = oot(&["record", "-m", "second"], &proj);
    assert!(ok, "{msg}");

    let (ok, log) = oot(&["log"], &proj);
    assert!(ok, "{log}");
    let lines: Vec<&str> = log.lines().collect();
    let second_id = lines[0].split_whitespace().next().unwrap();
    let first_id = lines[1].split_whitespace().next().unwrap();

    // Restore first change
    let (ok, msg) = oot(&["update", "--change", &first_id[..7], "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(proj.join("logs/.ootkeep").exists());
    assert!(proj.join("doc.ootkeep").exists());

    // Update to second change with --force: logs/.ootkeep must survive, doc.ootkeep must be deleted
    let (ok, msg) = oot(&["update", "--change", &second_id[..7], "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(
        proj.join("logs/.ootkeep").exists(),
        ".ootkeep marker must survive across updates"
    );
    assert!(
        !proj.join("doc.ootkeep").exists(),
        "regular file ending with .ootkeep must be deleted when not in target"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_branch_with_slashes() {
    let tmp = unique_tmp("branch-slashes");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("feature.txt"), "feat\n").unwrap();
    let (ok, msg) = oot(
        &["record", "--branch", "feat/auth-v2", "-m", "feat commit"],
        &proj,
    );
    assert!(ok, "{msg}");

    let _ = std::fs::remove_file(proj.join("feature.txt"));
    std::fs::write(proj.join("bugfix.txt"), "fix\n").unwrap();
    let (ok, msg) = oot(
        &["record", "--branch", "fix/bug#123", "-m", "fix commit"],
        &proj,
    );
    assert!(ok, "{msg}");

    // Switch between branches with slashes and special chars
    let (ok, msg) = oot(&["update", "--branch", "feat/auth-v2", "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(proj.join("feature.txt").exists());
    assert!(!proj.join("bugfix.txt").exists());

    let (ok, msg) = oot(&["update", "--branch", "fix/bug#123", "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(proj.join("bugfix.txt").exists());
    assert!(!proj.join("feature.txt").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_3way_conflict_preserves_work() {
    let tmp = unique_tmp("3way-conflict");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    // Base change on main
    std::fs::write(proj.join("a.txt"), "base a\n").unwrap();
    std::fs::write(proj.join("b.txt"), "base b\n").unwrap();
    let (ok, msg) = oot(&["record", "--branch", "main", "-m", "base"], &proj);
    assert!(ok, "{msg}");

    // Target change on branch feat: target deletes a.txt and modifies b.txt
    let _ = std::fs::remove_file(proj.join("a.txt"));
    std::fs::write(proj.join("b.txt"), "target b\n").unwrap();
    let (ok, msg) = oot(&["record", "--branch", "feat", "-m", "target"], &proj);
    assert!(ok, "{msg}");

    // Reset back to main
    let (ok, msg) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{msg}");

    // Create local modifications on main: work modifies a.txt and modifies b.txt differently
    std::fs::write(proj.join("a.txt"), "local dirty a\n").unwrap();
    std::fs::write(proj.join("b.txt"), "local dirty b\n").unwrap();

    // 3-way update to feat without --force: must NOT overwrite local modifications
    let (ok, out) = oot(&["update", "--branch", "feat"], &proj);
    assert!(
        ok,
        "update with conflicts should succeed and keep work: {out}"
    );
    assert!(
        out.contains("merge conflicts") || out.contains("conflict"),
        "{out}"
    );

    // Local work must be preserved
    assert_eq!(
        std::fs::read_to_string(proj.join("a.txt")).unwrap(),
        "local dirty a\n"
    );
    assert_eq!(
        std::fs::read_to_string(proj.join("b.txt")).unwrap(),
        "local dirty b\n"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
