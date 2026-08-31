//! Tests for Oot store garbage collection and pruning (`oot gc` / `oot prune`).

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

fn extract_id(record_output: &str) -> String {
    for line in record_output.lines() {
        if let Some(rest) = line.strip_prefix("recorded ") {
            if let Some(id) = rest.split_whitespace().next() {
                return id.to_string();
            }
        }
    }
    panic!("could not find recorded id in output: {record_output}");
}

#[test]
fn test_gc_prunes_orphaned_changes_and_dockets_on_force() {
    let tmp = std::env::temp_dir().join(format!("oot-gc-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // 1. Init store
    let (ok, msg) = oot(&["init"], &tmp);
    assert!(ok, "init failed: {msg}");

    // 2. Record change 1
    std::fs::write(tmp.join("file1.txt"), "hello v1\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "first commit"], &tmp);
    assert!(ok, "record 1 failed: {msg}");
    let c1 = extract_id(&msg);

    // 3. Record change 2
    std::fs::write(tmp.join("file2.txt"), "hello v2\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "second commit"], &tmp);
    assert!(ok, "record 2 failed: {msg}");
    let c2 = extract_id(&msg);

    // 4. Record change 3
    std::fs::write(tmp.join("file3.txt"), "hello v3\n").unwrap();
    let (ok, msg) = oot(&["record", "-m", "third commit"], &tmp);
    assert!(ok, "record 3 failed: {msg}");
    let c3 = extract_id(&msg);

    // Adjudicate change 3 to persist a docket
    let (ok, msg) = oot(&["adjudicate", "--change", &c3], &tmp);
    assert!(ok, "adjudicate failed: {msg}");
    assert!(tmp.join(format!(".oot/dockets/{c3}.json")).exists());

    // 5. Rewind branch ref to change 1 (making c2 and c3 unreferenced orphans)
    std::fs::write(tmp.join(".oot/refs/main"), format!("{c1}\n")).unwrap();

    // 6. Test dry run: identifies 2 changes and 1 docket, but does not delete
    let (ok, out) = oot(&["gc", "--dry-run", "--force"], &tmp);
    assert!(ok, "gc dry-run failed: {out}");
    assert!(
        out.contains("eligible changes:  2"),
        "dry run mismatch: {out}"
    );
    assert!(
        out.contains("eligible dockets:  1"),
        "dry run mismatch: {out}"
    );
    assert!(tmp.join(format!(".oot/changes/{c2}.json")).exists());
    assert!(tmp.join(format!(".oot/changes/{c3}.json")).exists());
    assert!(tmp.join(format!(".oot/dockets/{c3}.json")).exists());

    // 7. Test force GC: deletes orphaned changes and dockets, compacts index
    let (ok, out) = oot(&["gc", "--force"], &tmp);
    assert!(ok, "gc force failed: {out}");
    assert!(
        out.contains("pruned changes:  2"),
        "gc output mismatch: {out}"
    );
    assert!(
        out.contains("pruned dockets:  1"),
        "gc output mismatch: {out}"
    );
    assert!(
        out.contains("live changes:    1"),
        "gc output mismatch: {out}"
    );

    assert!(
        tmp.join(format!(".oot/changes/{c1}.json")).exists(),
        "live change c1 must exist"
    );
    assert!(
        !tmp.join(format!(".oot/changes/{c2}.json")).exists(),
        "c2 should be pruned"
    );
    assert!(
        !tmp.join(format!(".oot/changes/{c3}.json")).exists(),
        "c3 should be pruned"
    );
    assert!(
        !tmp.join(format!(".oot/dockets/{c3}.json")).exists(),
        "c3 docket should be pruned"
    );

    // Index only contains c1
    let index = std::fs::read_to_string(tmp.join(".oot/.index")).unwrap();
    assert_eq!(index.trim(), c1);

    // 8. Verify store operations still work cleanly
    let (ok, log) = oot(&["log"], &tmp);
    assert!(ok, "log failed: {log}");
    assert!(log.contains("first commit"));
    assert!(!log.contains("second commit"));

    let out_dir = tmp.join("exported");
    let (ok, msg) = oot(&["export", "--out", out_dir.to_str().unwrap()], &tmp);
    assert!(ok, "export after gc failed: {msg}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_gc_grace_period_and_prune_alias() {
    let tmp = std::env::temp_dir().join(format!("oot-gc-grace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    assert!(oot(&["init"], &tmp).0);

    std::fs::write(tmp.join("file.txt"), "v1\n").unwrap();
    let (_, msg) = oot(&["record", "-m", "c1"], &tmp);
    let c1 = extract_id(&msg);

    std::fs::write(tmp.join("file.txt"), "v2\n").unwrap();
    let (_, msg) = oot(&["record", "-m", "c2"], &tmp);
    let c2 = extract_id(&msg);

    // Rewind ref to c1
    std::fs::write(tmp.join(".oot/refs/main"), format!("{c1}\n")).unwrap();

    // Default gc with grace period should NOT prune c2 because it was just created
    let (ok, out) = oot(&["gc"], &tmp);
    assert!(ok, "default gc failed: {out}");
    assert!(
        out.contains("pruned changes:  0"),
        "grace period should preserve fresh orphan: {out}"
    );
    assert!(tmp.join(format!(".oot/changes/{c2}.json")).exists());

    // Prune alias with --force should immediately prune it
    let (ok, out) = oot(&["prune", "--force"], &tmp);
    assert!(ok, "prune alias failed: {out}");
    assert!(
        out.contains("pruned changes:  1"),
        "prune --force should sweep orphan: {out}"
    );
    assert!(!tmp.join(format!(".oot/changes/{c2}.json")).exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_gc_preserves_unexpired_odb_objects_and_allows_resurrection() {
    let tmp = std::env::temp_dir().join(format!("oot-gc-odb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    assert!(oot(&["init"], &tmp).0);

    // c1: root commit
    std::fs::write(tmp.join("file1.txt"), "hello v1\n").unwrap();
    let (_, msg) = oot(&["record", "-m", "c1"], &tmp);
    let c1 = extract_id(&msg);

    // c2: older commit off c1
    std::fs::write(tmp.join("file2.txt"), "hello v2\n").unwrap();
    let (_, msg) = oot(&["record", "-m", "c2"], &tmp);
    let c2 = extract_id(&msg);

    // Rewind back to c1 before creating c3 so c3's parent is c1 (independent branch)
    std::fs::write(tmp.join(".oot/refs/main"), format!("{c1}\n")).unwrap();
    std::fs::remove_file(tmp.join("file2.txt")).unwrap();

    // c3: unexpired orphan change off c1
    std::fs::write(tmp.join("file3.txt"), "hello v3 unexpired\n").unwrap();
    let (_, msg) = oot(&["record", "-m", "c3"], &tmp);
    let c3 = extract_id(&msg);

    // Backdate c2's file mtime to 30 days ago
    let c2_path = tmp.join(format!(".oot/changes/{c2}.json"));
    let status = std::process::Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(&c2_path)
        .status()
        .unwrap();
    assert!(status.success());

    // Rewind main ref to c1 (so c2 and c3 are both unreferenced)
    std::fs::write(tmp.join(".oot/refs/main"), format!("{c1}\n")).unwrap();

    // Run GC with 14d expiration: c2 is expired and gets pruned; c3 is fresh and kept!
    let (ok, out) = oot(&["gc", "--expire", "14d"], &tmp);
    assert!(ok, "gc failed: {out}");
    assert!(
        out.contains("pruned changes:  1"),
        "should prune exactly 1 expired change: {out}"
    );
    assert!(!c2_path.exists(), "c2 should be deleted from changes");
    assert!(
        tmp.join(format!(".oot/changes/{c3}.json")).exists(),
        "c3 should be preserved"
    );

    // c3 must still be present in .index
    let index = std::fs::read_to_string(tmp.join(".oot/.index")).unwrap();
    assert!(
        index.contains(&c3),
        ".index must contain unexpired change c3"
    );
    assert!(
        !index.contains(&c2),
        ".index must not contain pruned change c2"
    );

    // Crucial check: verify that c3's blobs and trees survived Git ODB compaction
    let (ok, msg) = oot(&["update", "--change", &c3, "--force"], &tmp);
    assert!(ok, "update to unexpired change failed: {msg}");
    assert_eq!(
        std::fs::read_to_string(tmp.join("file3.txt")).unwrap(),
        "hello v3 unexpired\n"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
