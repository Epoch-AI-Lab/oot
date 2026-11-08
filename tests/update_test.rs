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
    // Already at head must take the fast path and not rewrite.
    assert!(
        out.contains("already up to date"),
        "clean update at head should be a no-op: {out}"
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
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "symlink entry must survive update"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), outside);
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

    // Two crafted changes sharing a prefix make resolve_change's ambiguous
    // branch observable through the CLI (the real "first" id is a hash and
    // does not collide with this prefix).
    let changes = proj.join(".oot/changes");
    for tail in ["1", "2"] {
        let id = format!("aaaa00000000000000000000000000000000000{tail}");
        let record = r#"{"parents":[],"tree":"4b825dc642cb6eb9a060e54bf8d69288fbee4904","author":{"name":"K","email":"k@oot.dev","time":0,"offset":"+0000"},"committer":{"name":"K","email":"k@oot.dev","time":0,"offset":"+0000"},"message":"crafted\n","source_sha":null}"#.to_string();
        std::fs::write(changes.join(format!("{id}.json")), record)
            .unwrap_or_else(|e| panic!("write crafted change {id}: {e}"));
    }
    let (ok, out) = oot(&["update", "--change", "aaaa"], &proj);
    assert!(!ok, "{out}");
    assert!(out.contains("ambiguous change prefix 'aaaa'"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_rejects_path_traversal() {
    let tmp = unique_tmp("traversal");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    // Valid nested path must be accepted (no false rejection).
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

    // Defense-in-depth: a tree carrying a traversal path must never be
    // materialized by `oot update`, even if git --literally was used to
    // smuggle it into the odb past fsck.
    use oot::store::validate_tree_path;
    assert!(validate_tree_path("../evil.txt").is_err());
    assert!(validate_tree_path("a/../evil.txt").is_err());
    assert!(validate_tree_path("/abs/evil.txt").is_err());
    assert!(validate_tree_path("a//b.txt").is_err());
    assert!(validate_tree_path(".git/config").is_err());

    let store = oot::store::Store::open(&proj).expect("store should open");
    let valid = store
        .get_change(&store.resolve_change(id).expect("resolve valid prefix"))
        .expect("valid change should load");
    let blob_sha = {
        use std::io::Write;
        let mut child = Command::new("git")
            .args(["--git-dir"])
            .arg(store.git_dir())
            .args(["hash-object", "-t", "blob", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("git hash-object blob should spawn");
        child
            .stdin
            .take()
            .expect("blob stdin should exist")
            .write_all(b"evil\n")
            .expect("write blob");
        let out = child.wait_with_output().expect("wait for blob");
        assert!(
            out.status.success(),
            "git hash-object blob failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let blob_bytes: Vec<u8> = (0..blob_sha.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&blob_sha[i..i + 2], 16).expect("blob sha hex"))
        .collect();
    assert_eq!(blob_bytes.len(), 20);

    // Each entry would escape or enter a VCS dir if written verbatim.
    let escape_outside = tmp.join("evil.txt");
    for evil_name in ["../evil.txt", ".git/evil.txt", "a//b.txt"] {
        let mut tree_bytes = Vec::new();
        tree_bytes.extend_from_slice(b"100644 ");
        tree_bytes.extend_from_slice(evil_name.as_bytes());
        tree_bytes.push(0);
        tree_bytes.extend_from_slice(&blob_bytes);
        let tree_sha = {
            use std::io::Write;
            let mut child = Command::new("git")
                .args(["--git-dir"])
                .arg(store.git_dir())
                .args(["hash-object", "-t", "tree", "-w", "--literally", "--stdin"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("git hash-object tree should spawn");
            child
                .stdin
                .take()
                .expect("tree stdin should exist")
                .write_all(&tree_bytes)
                .expect("write tree");
            let out = child.wait_with_output().expect("wait for tree");
            assert!(
                out.status.success(),
                "git hash-object --literally failed for {evil_name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let mut evil = valid.clone();
        evil.tree = tree_sha;
        evil.message = format!("evil {evil_name}");
        let evil_id = store.put_record(&evil).expect("put evil record");
        let (ok, out) = oot(&["update", "--change", evil_id.as_str(), "--force"], &proj);
        assert!(!ok, "update must reject tree path '{evil_name}': {out}");
        assert!(
            out.contains("invalid path in tree"),
            "rejection must name the invalid path: {out}"
        );
        assert!(
            !escape_outside.exists(),
            "traversal must not write outside the repo"
        );
    }
    assert!(!proj.join(".git/evil.txt").exists());
    assert_eq!(
        std::fs::read_to_string(proj.join("a/b/c.txt")).unwrap(),
        "ok\n"
    );

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
    // After downgrading to the old change, the working copy differs from the
    // branch head (second). Status must report the files as modified.
    assert!(
        out.contains("M a.txt"),
        "status should report M a.txt: {out}"
    );
    assert!(
        out.contains("M sub/b.txt"),
        "status should report M sub/b.txt: {out}"
    );
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

    // First tree has NEITHER marker; second adds both the exact marker and a
    // suffix lookalike (both tracked). UPDATE-08: a tracked marker deleted by
    // the target follows the target like any other file, so a clean
    // fast-forward back to first removes logs/.ootkeep. The suffix file
    // doc.ootkeep is not a keep marker (is_keep_marker matches exact basenames
    // .ootkeep/.oothave only) and must also be deleted.
    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    std::fs::create_dir_all(proj.join("logs")).unwrap();
    std::fs::write(proj.join("logs/.ootkeep"), "").unwrap();
    std::fs::write(proj.join("doc.ootkeep"), "regular text file\n").unwrap();
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

    // Clean fast-forward without --force: work == head == second, target
    // (first) lacks both files. Tracked markers follow the target (UPDATE-08);
    // the suffix lookalike is not a keep marker.
    let (ok, msg) = oot(&["update", "--change", first_id], &proj);
    assert!(ok, "{msg}");
    assert!(
        !proj.join("logs/.ootkeep").exists(),
        "tracked .ootkeep deleted by target must not survive a clean fast-forward (UPDATE-08)"
    );
    assert!(
        !proj.join("doc.ootkeep").exists(),
        "suffix doc.ootkeep is not a keep marker and must be deleted when not in target"
    );

    // Second project: neither tree contains the marker. A fresh untracked
    // marker must survive a non-force update while tracked content moves.
    let proj2 = tmp.join("proj2");
    std::fs::create_dir_all(&proj2).unwrap();
    assert!(oot(&["init"], &proj2).0);
    std::fs::write(proj2.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first"], &proj2);
    assert!(ok, "{msg}");
    std::fs::write(proj2.join("a.txt"), "v2\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "second"], &proj2);
    assert!(ok, "{msg}");
    std::fs::create_dir_all(proj2.join("logs")).unwrap();
    std::fs::write(proj2.join("logs/.ootkeep"), "").unwrap();
    let (ok, log2) = oot(&["log"], &proj2);
    assert!(ok, "{log2}");
    let first2_id = log2
        .lines()
        .last()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let (ok, msg) = oot(&["update", "--change", first2_id], &proj2);
    assert!(ok, "untracked marker update should merge: {msg}");
    assert!(
        proj2.join("logs/.ootkeep").exists(),
        "untracked .ootkeep must survive a non-force update"
    );
    assert_eq!(
        std::fs::read_to_string(proj2.join("a.txt")).unwrap(),
        "v1\n"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_deletes_tracked_keep_marker_on_fast_forward() {
    // UPDATE-08: a tracked .ootkeep/.oothave deleted by the target must be
    // removed on a clean fast-forward. Preserving it makes a successful update
    // leave the worktree dirty.
    let tmp = unique_tmp("tracked-keepmarker-ff");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::create_dir_all(proj.join("logs")).unwrap();
    std::fs::write(proj.join("logs/.ootkeep"), "").unwrap();
    std::fs::write(proj.join("file.txt"), "base\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "add marker"], &proj);
    assert!(ok, "{msg}");

    // Remove the tracked marker and record its deletion on a separate branch.
    std::fs::remove_file(proj.join("logs/.ootkeep")).unwrap();
    let (ok, msg) = oot(
        &["record", "--branch", "feat", "-m", "remove marker"],
        &proj,
    );
    assert!(ok, "{msg}");
    assert!(!proj.join("logs/.ootkeep").exists());

    let (ok, log) = oot(&["log", "--branch", "feat"], &proj);
    assert!(ok, "{log}");
    let without_marker_id = log
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    // Return to main (marker present, tracked) as a clean single-branch state.
    let (ok, msg) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{msg}");
    assert!(proj.join("logs/.ootkeep").exists());
    // Keep only main so resolve_branch(None) can infer the head/base.
    // oot stores branch refs as flat files in .oot/refs/ (not refs/heads/).
    let _ = std::fs::remove_file(proj.join(".oot/refs/feat"));

    // Clean fast-forward to the change that deleted the tracked marker (no --force).
    let (ok, msg) = oot(&["update", "--change", &without_marker_id[..7]], &proj);
    assert!(ok, "update should succeed: {msg}");
    assert!(
        !msg.contains("merged"),
        "expected clean fast-forward path, got: {msg}"
    );
    assert!(
        !proj.join("logs/.ootkeep").exists(),
        "tracked marker deleted by target must not survive a clean fast-forward: {msg}"
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
fn test_update_long_unicode_filename_does_not_panic() {
    let tmp = unique_tmp("unicode-name");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "--branch", "main", "-m", "first"], &proj);
    assert!(ok, "{msg}");

    // The long name lives on another branch, so update has to write it.
    // 127 ASCII bytes then e-acute at bytes 127..129, so byte index 128
    // lands inside the character.
    let long_name = format!("{}\u{e9}", "a".repeat(127));
    assert!(long_name.len() > 128);
    assert!(!long_name.is_char_boundary(128));
    std::fs::write(proj.join(&long_name), "payload\n").unwrap();
    let (ok, msg) = oot(&["record", "--branch", "feat", "-m", "long name"], &proj);
    assert!(ok, "{msg}");

    // Return to main (clean) so updating to feat is a clean fast-forward that
    // materializes the long-named file through the straight write path.
    let (ok, msg) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{msg}");
    let _ = std::fs::remove_file(proj.join(&long_name));

    let (ok, out) = oot(&["update", "--branch", "feat"], &proj);
    assert!(
        ok,
        "update must not panic on a long unicode filename: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(proj.join(&long_name)).unwrap_or_default(),
        "payload\n",
        "the long-named file must land on disk: {out}"
    );
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v1\n");

    // Same path under --force.
    let (ok, out) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{out}");
    let (ok, out) = oot(&["update", "--branch", "feat", "--force"], &proj);
    assert!(ok, "forced update must not panic either: {out}");
    assert_eq!(
        std::fs::read_to_string(proj.join(&long_name)).unwrap(),
        "payload\n"
    );

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

#[test]
fn test_update_multi_branch_clean_ff_advances_head_update01_02() {
    let tmp = unique_tmp("multi-ff-head");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "v1-main"], &proj);
    assert!(ok, "{msg}");

    std::fs::write(proj.join("a.txt"), "v2\n").unwrap();
    let (ok, msg) = oot(&["record", "--branch", "feat", "-m", "v2-feat"], &proj);
    assert!(ok, "{msg}");

    // Clean checkout at main.
    let (ok, msg) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{msg}");
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v1\n");
    let head = std::fs::read_to_string(proj.join(".oot/HEAD")).unwrap();
    assert!(
        head.contains("refs/heads/main"),
        "HEAD should point to main after force update: {head}"
    );

    // UPDATE-01: clean fast-forward to feat in a multi-branch store must take
    // target content, not report a false conflict.
    let (ok, out) = oot(&["update", "--branch", "feat"], &proj);
    assert!(ok, "clean ff to feat should succeed: {out}");
    assert!(
        !out.contains("conflict"),
        "UPDATE-01: clean fast-forward reported false conflict: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(proj.join("a.txt")).unwrap(),
        "v2\n",
        "UPDATE-01: feat content not materialized"
    );

    // UPDATE-02: HEAD must advance on merge/materialized branch update paths.
    let head = std::fs::read_to_string(proj.join(".oot/HEAD")).unwrap();
    assert!(
        head.contains("refs/heads/feat"),
        "UPDATE-02: HEAD not advanced to feat: {head}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_already_up_to_date_advances_head_update02() {
    let tmp = unique_tmp("uptodate-head");
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(oot(&["init"], &proj).0);

    std::fs::write(proj.join("a.txt"), "v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "v1"], &proj);
    assert!(ok, "{msg}");

    std::fs::write(proj.join("a.txt"), "v2\n").unwrap();
    let (ok, msg) = oot(&["record", "--branch", "feat", "-m", "v2"], &proj);
    assert!(ok, "{msg}");

    // HEAD is feat (record set it). Make work identical to main without moving HEAD.
    let (ok, msg) = oot(&["update", "--branch", "main", "--force"], &proj);
    assert!(ok, "{msg}");
    std::fs::write(proj.join(".oot/HEAD"), "ref: refs/heads/feat\n").unwrap();
    assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v1\n");

    let (ok, out) = oot(&["update", "--branch", "main"], &proj);
    assert!(ok, "{out}");
    assert!(
        out.contains("already up to date"),
        "expected already up to date: {out}"
    );
    let head = std::fs::read_to_string(proj.join(".oot/HEAD")).unwrap();
    assert!(
        head.contains("refs/heads/main"),
        "UPDATE-02: already-up-to-date did not advance HEAD: {head}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
