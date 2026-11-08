//! Mutating commands take the store lock: with the lock held by a live
//! holder, `oot gc --force` must wait and then give up instead of
//! mutating the store unlocked (X-05). Record always took the lock; gc
//! used to run entirely without it, racing other writers.

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

#[test]
fn test_gc_waits_for_store_lock() {
    let tmp = std::env::temp_dir().join(format!("oot-lock-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    let (ok, msg) = oot(&["init"], &proj);
    assert!(ok, "init failed: {msg}");

    // Hold the store lock as a live holder (this test process).
    let lock_path = proj.join(".oot/lock");
    std::fs::write(&lock_path, format!("{}\n", std::process::id())).unwrap();

    let (ok, msg) = oot(&["gc", "--force"], &proj);
    assert!(!ok, "gc must not run unlocked: {msg}");
    assert!(
        msg.contains("timed out waiting for store lock"),
        "gc must fail waiting for the lock: {msg}"
    );

    // The lock file is untouched, and gc succeeds once it is gone.
    assert!(lock_path.exists(), "gc must not remove a live lock");
    std::fs::remove_file(&lock_path).unwrap();
    let (ok, msg) = oot(&["gc", "--force"], &proj);
    assert!(ok, "gc after release failed: {msg}");

    let _ = std::fs::remove_dir_all(&tmp);
}
