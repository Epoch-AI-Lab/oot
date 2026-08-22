//! Integration tests for the Jujutsu adapter.
//!
//! These run against hermetic `jj` repositories created in the system temp dir.
//! They skip (pass trivially) when the `jj` binary is not installed.

use oot::adapter::{JjAdapter, JjAdjudicateOptions};
use oot::engine::Engine;
use oot::policy::MeaningPolicy;
use oot::visibility::VisibilityPolicy;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct TempRepo {
    path: PathBuf,
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Run a mutating jj command in the repo; panics on failure.
fn jj(dir: &Path, args: &[&str]) -> String {
    let mut full: Vec<&str> = vec![
        "--no-pager",
        "--config",
        "user.name=Oot Test",
        "--config",
        "user.email=oot@example.com",
    ];
    full.extend_from_slice(args);

    let out = Command::new("jj")
        .args(&full)
        .current_dir(dir)
        .output()
        .expect("failed to spawn jj");
    assert!(
        out.status.success(),
        "jj {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo(colocate: bool) -> TempRepo {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("oot-jj-test-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    if colocate {
        jj(&dir, &["git", "init", "--colocate", "."]);
    } else {
        jj(&dir, &["git", "init", "--no-colocate", "."]);
    }
    TempRepo { path: dir }
}

fn write_lib(dir: &Path, body: &str) {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        format!("pub fn hello() -> &'static str {{ \"{}\" }}\n", body),
    )
    .unwrap();
}

/// Create a repo with two commits: `main` (base) and a head commit on top.
/// Returns the repo plus the revset for the head commit (`@-` after setup).
fn setup_base_and_head(colocate: bool) -> (TempRepo, JjAdapter) {
    let repo = init_repo(colocate);
    write_lib(&repo.path, "hello");

    // Commit base, bookmark it as main. Working copy becomes a new empty @.
    jj(&repo.path, &["commit", "-m", "base"]);
    jj(&repo.path, &["bookmark", "create", "main", "-r", "@-"]);

    // Head commit modifies the function body.
    write_lib(&repo.path, "hello world");
    jj(&repo.path, &["commit", "-m", "change"]);

    let adapter = JjAdapter::new(&repo.path).expect("adapter should discover jj repo");
    (repo, adapter)
}

#[test]
fn test_jj_adapter_discover_and_root() {
    if !jj_available() {
        return;
    }
    for colocate in [true, false] {
        let (repo, adapter) = setup_base_and_head(colocate);
        assert!(adapter.repo_root().exists());
        assert!(adapter.repo_root().starts_with(std::env::temp_dir()));
        drop(repo);
    }
}

#[test]
fn test_jj_resolve_commit_id() {
    if !jj_available() {
        return;
    }
    let (_repo, adapter) = setup_base_and_head(false);

    let id = adapter.resolve_commit_id("@-").expect("head resolves");
    assert!(!id.is_empty());

    let main_id = adapter
        .resolve_commit_id("bookmarks(exact:main)")
        .expect("bookmark resolves");
    assert_ne!(id, main_id, "base and head are distinct commits");

    assert!(adapter.resolve_commit_id("no-such-bookmark-xyz").is_err());
}

#[test]
fn test_jj_ancestor_of_base_and_head_is_base() {
    if !jj_available() {
        return;
    }
    let (_repo, adapter) = setup_base_and_head(true);

    let base = adapter.resolve_commit_id("bookmarks(exact:main)").unwrap();
    let head = adapter.resolve_commit_id("@-").unwrap();
    let anc = adapter.ancestor(&base, &head).expect("ancestor exists");
    assert_eq!(anc, base, "linear history: ancestor of main..@- is main");
}

#[test]
fn test_jj_extract_snapshot() {
    if !jj_available() {
        return;
    }
    let (_repo, adapter) = setup_base_and_head(false);

    let snap = adapter.extract_snapshot("@-").expect("snapshot extracts");
    let content = String::from_utf8_lossy(
        snap.files
            .get("src/lib.rs")
            .expect("src/lib.rs present in head snapshot"),
    );
    assert!(content.contains("hello world"));
    assert!(!content.contains("<<<<<<<"), "clean commit has no markers");
}

#[test]
fn test_jj_extract_snapshot_binary_exact_bytes() {
    if !jj_available() {
        return;
    }
    let repo = init_repo(false);
    write_lib(&repo.path, "hello");
    jj(&repo.path, &["commit", "-m", "base"]);

    // Invalid UTF-8: pins byte-exact file storage through `jj file show`
    // (lossy conversion would turn 0xFF into U+FFFD and fail this assert).
    std::fs::write(repo.path.join("assets.bin"), [0xFFu8, 0x00, 0x81]).unwrap();
    jj(&repo.path, &["commit", "-m", "add binary"]);

    let adapter = JjAdapter::new(&repo.path).expect("adapter should discover jj repo");
    let snap = adapter.extract_snapshot("@").expect("snapshot extracts");
    assert_eq!(
        snap.files.get("assets.bin").expect("binary present"),
        &[0xFF, 0x00, 0x81]
    );
}

#[test]
fn test_jj_adjudicate_3way_clean_unilateral_change() {
    if !jj_available() {
        return;
    }
    let (_repo, adapter) = setup_base_and_head(false);
    let eng = Engine::new().unwrap();

    let docket = adapter
        .adjudicate_3way(
            "bookmarks(exact:main)",
            "@-",
            &eng,
            &MeaningPolicy::default(),
            &VisibilityPolicy::default(),
            &JjAdjudicateOptions {
                change_name: Some("test-change".into()),
                ..Default::default()
            },
        )
        .expect("adjudication succeeds");

    assert_eq!(docket.change, "test-change");
    assert!(docket.source.starts_with("jj:"), "docket labeled jj");
    assert!(
        !docket.disputes.is_empty(),
        "modified function should raise a meaning dispute"
    );
    assert!(docket
        .disputes
        .iter()
        .any(|d| d.detail.contains("hello") && d.kind == oot::dispute::Kind::Meaning));
    assert_eq!(docket.verdict, oot::dispute::Verdict::Adjudicated);

    let rendered = docket.render();
    assert!(rendered.contains("OOT DOCKET"));
}

#[test]
fn test_jj_conflicted_commit_detected_not_parsed() {
    if !jj_available() {
        return;
    }
    let repo = init_repo(false);
    write_lib(&repo.path, "original");

    jj(&repo.path, &["commit", "-m", "base"]);
    jj(&repo.path, &["bookmark", "create", "main", "-r", "@-"]);

    // Side A changes the line one way.
    write_lib(&repo.path, "side a");
    jj(&repo.path, &["commit", "-m", "side a"]);
    let adapter = JjAdapter::new(&repo.path).unwrap();
    let side_a = adapter.resolve_commit_id("@-").unwrap();

    // Side B starts from main and changes the same line differently.
    jj(&repo.path, &["new", "bookmarks(exact:main)"]);
    write_lib(&repo.path, "side b");
    jj(&repo.path, &["commit", "-m", "side b"]);
    let side_b = adapter.resolve_commit_id("@-").unwrap();

    // Merge both sides: conflicting edit to the same line yields a first-class conflict.
    jj(&repo.path, &["new", &side_a, &side_b]);

    let (snap, conflicted) = adapter
        .extract_snapshot_with_conflicts("@")
        .expect("merge snapshot extracts");

    assert!(
        conflicted.iter().any(|p| p == "src/lib.rs"),
        "conflicted file should be reported separately, got {:?}",
        conflicted
    );
    assert!(
        !snap.files.contains_key("src/lib.rs"),
        "conflicted file must be excluded from the parseable snapshot"
    );
}
