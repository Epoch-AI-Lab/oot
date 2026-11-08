//! Integration tests for the Git 3-Way Snapshot Ingestion Adapter.

use oot::adapter::{GitAdapter, GitAdjudicateOptions};
use oot::dispute::{Kind, Severity, Verdict, EMPTY_CHANGE_ID};
use oot::engine::Engine;
use oot::policy::MeaningPolicy;
use oot::visibility::VisibilityPolicy;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Helper struct to create and clean up temporary git repositories.
struct TempGitRepo {
    path: PathBuf,
}

impl TempGitRepo {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("oot_git_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp git dir");

        // git init
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&path)
            .status()
            .expect("failed to init git");
        assert!(status.success());

        // configure identity for commits
        let _ = Command::new("git")
            .args(["config", "user.name", "Oot Tester"])
            .current_dir(&path)
            .status();
        let _ = Command::new("git")
            .args(["config", "user.email", "tester@oot.local"])
            .current_dir(&path)
            .status();

        Self { path }
    }

    fn write_file(&self, rel_path: &str, content: &str) {
        self.write_bytes(rel_path, content.as_bytes());
    }

    fn write_bytes(&self, rel_path: &str, content: &[u8]) {
        let full = self.path.join(rel_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("failed to create parent dir");
        }
        fs::write(full, content).expect("failed to write test file");
    }

    fn commit(&self, msg: &str) -> String {
        let s1 = Command::new("git")
            .args(["add", "."])
            .current_dir(&self.path)
            .status()
            .expect("git add failed");
        assert!(s1.success());

        let s2 = Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(&self.path)
            .status()
            .expect("git commit failed");
        assert!(s2.success());

        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.path)
            .output()
            .expect("git rev-parse HEAD failed");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn create_and_checkout_branch(&self, branch: &str) {
        let status = Command::new("git")
            .args(["checkout", "-b", branch])
            .current_dir(&self.path)
            .status()
            .expect("git checkout -b failed");
        assert!(status.success());
    }

    fn checkout(&self, branch_or_rev: &str) {
        let status = Command::new("git")
            .args(["checkout", branch_or_rev])
            .current_dir(&self.path)
            .status()
            .expect("git checkout failed");
        assert!(status.success());
    }

    /// Commit under a specific author identity, so docket provenance is real.
    fn commit_as(&self, msg: &str, name: &str, email: &str) -> String {
        let prev_name = self.git_config("user.name");
        let prev_email = self.git_config("user.email");
        self.set_git_config("user.name", name);
        self.set_git_config("user.email", email);
        let sha = self.commit(msg);
        self.set_git_config("user.name", &prev_name);
        self.set_git_config("user.email", &prev_email);
        sha
    }

    fn git_config(&self, key: &str) -> String {
        let out = Command::new("git")
            .args(["config", "--get", key])
            .current_dir(&self.path)
            .output()
            .expect("git config read failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn set_git_config(&self, key: &str, value: &str) {
        let status = Command::new("git")
            .args(["config", key, value])
            .current_dir(&self.path)
            .status()
            .expect("git config write failed");
        assert!(status.success());
    }
}

impl Drop for TempGitRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_git_adapter_snapshot_extraction() {
    let repo = TempGitRepo::new("snapshot_extract");
    repo.write_file(
        "src/lib.rs",
        "pub fn compute() -> i32 { 42 }\n\npub fn auth() -> bool { true }\n",
    );
    repo.write_file("README.md", "# Test Repo\n");
    // Invalid UTF-8: pins byte-exact blob storage (lossy conversion would
    // turn 0xFF into U+FFFD and this assertion would fail).
    repo.write_bytes("assets/blob.bin", &[0xFF, 0x00, 0x81]);
    let c1 = repo.commit("initial commit");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let snapshot = adapter.extract_snapshot(&c1).expect("extract snapshot");

    assert_eq!(snapshot.files.len(), 3);
    assert!(snapshot.files.contains_key("src/lib.rs"));
    assert!(snapshot.files.contains_key("README.md"));
    assert_eq!(
        snapshot.files.get("src/lib.rs").unwrap(),
        "pub fn compute() -> i32 { 42 }\n\npub fn auth() -> bool { true }\n".as_bytes()
    );
    assert_eq!(
        snapshot.files.get("assets/blob.bin").unwrap(),
        &[0xFF, 0x00, 0x81]
    );
}

#[test]
fn test_git_adapter_3way_semantic_conflict() {
    let repo = TempGitRepo::new("3way_conflict");

    // 1. Initial base commit on main
    repo.write_file(
        "src/lib.rs",
        "pub fn common() -> i32 { 0 }\npub fn target_fn() -> &'static str { \"v1\" }\n",
    );
    let base_sha = repo.commit("base version");

    // 2. Feature branch: modifies target_fn to "feature_v2"
    repo.create_and_checkout_branch("feature/auth");
    repo.write_file(
        "src/lib.rs",
        "pub fn common() -> i32 { 0 }\npub fn target_fn() -> &'static str { \"feature_v2\" }\n",
    );
    let _feature_sha = repo.commit("feature change");

    // 3. Main branch: modifies target_fn to "main_v2" (divergent!)
    repo.checkout("main");
    repo.write_file(
        "src/lib.rs",
        "pub fn common() -> i32 { 0 }\npub fn target_fn() -> &'static str { \"main_v2\" }\n",
    );
    let _main_sha = repo.commit("main change");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let engine = Engine::new().expect("valid engine");
    let meaning_policy = MeaningPolicy::default();
    let visibility_policy = VisibilityPolicy::default();

    // Check merge base
    let mb = adapter
        .merge_base("main", "feature/auth")
        .expect("merge-base");
    assert_eq!(mb, base_sha);

    // Run 3-way adjudication
    let options = GitAdjudicateOptions {
        custom_merge_base: None,
        change_name: Some("auth-3way".into()),
        intent: Some("refactor target_fn".into()),
        authors: None,
    };

    let docket = adapter
        .adjudicate_3way(
            "main",
            "feature/auth",
            &engine,
            &meaning_policy,
            &visibility_policy,
            &options,
        )
        .expect("adjudicate 3way");

    assert_eq!(docket.verdict, Verdict::Blocked);
    assert_eq!(docket.meaning_count(), 1);

    let dispute = &docket.disputes[0];
    assert_eq!(dispute.kind, Kind::Meaning);
    assert_eq!(dispute.severity, Severity::High);
    assert!(dispute.detail.contains("3-way conflict"));
    assert!(dispute.detail.contains("target_fn"));
}

#[test]
fn test_git_adapter_3way_unilateral_clean() {
    let repo = TempGitRepo::new("3way_clean");

    // Base commit on main
    repo.write_file(
        "src/lib.rs",
        "pub fn common() -> i32 { 0 }\npub fn helper() -> bool { false }\n",
    );
    repo.commit("base version");

    // Feature branch: adds a new function, keeps others unchanged
    repo.create_and_checkout_branch("feature/new-fn");
    repo.write_file(
        "src/lib.rs",
        "pub fn common() -> i32 { 0 }\npub fn helper() -> bool { false }\npub fn added_fn() -> i32 { 100 }\n",
    );
    repo.commit("feature added function");

    // Main branch: untouched
    repo.checkout("main");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let engine = Engine::new().expect("valid engine");
    let meaning_policy = MeaningPolicy::default();
    let visibility_policy = VisibilityPolicy::default();

    let docket = adapter
        .adjudicate_3way(
            "main",
            "feature/new-fn",
            &engine,
            &meaning_policy,
            &visibility_policy,
            &GitAdjudicateOptions::default(),
        )
        .expect("adjudicate 3way");

    assert_eq!(docket.verdict, Verdict::Adjudicated);
    assert_eq!(docket.meaning_count(), 1);
    assert_eq!(docket.disputes[0].severity, Severity::Low);
    assert!(docket.disputes[0]
        .detail
        .contains("added function `added_fn`"));
}

#[test]
fn test_git_adapter_visibility_violation_cloaked() {
    let repo = TempGitRepo::new("visibility_git");

    repo.write_file("src/lib.rs", "pub fn ok() {}\n");
    repo.commit("initial");

    repo.create_and_checkout_branch("feature/secrets");
    repo.write_file("secrets/api_keys.json", "{ \"key\": \"12345\" }\n");
    repo.commit("add secrets");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let engine = Engine::new().expect("valid engine");
    let meaning_policy = MeaningPolicy::default();
    let visibility_policy = VisibilityPolicy::default(); // defaults to secrets/ and .env private

    let docket = adapter
        .adjudicate_3way(
            "main",
            "feature/secrets",
            &engine,
            &meaning_policy,
            &visibility_policy,
            &GitAdjudicateOptions::default(),
        )
        .expect("adjudicate 3way");

    assert_eq!(docket.verdict, Verdict::Cloaked);
    assert_eq!(docket.visibility_count(), 1);
    assert!(docket.disputes[0].detail.contains("private path"));
}

#[test]
fn test_git_adapter_3way_reports_real_commit_authors() {
    let repo = TempGitRepo::new("3way_authors");

    repo.write_file("src/lib.rs", "pub fn common() -> i32 { 0 }\n");
    repo.commit_as("base version", "Base Author", "base@example.com");

    repo.create_and_checkout_branch("feature/auth");
    repo.write_file("src/lib.rs", "pub fn common() -> i32 { 1 }\n");
    repo.commit_as("feature commit", "Dana Feature", "dana@example.com");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let engine = Engine::new().expect("valid engine");

    let docket = adapter
        .adjudicate_3way(
            "main",
            "feature/auth",
            &engine,
            &MeaningPolicy::default(),
            &VisibilityPolicy::default(),
            &GitAdjudicateOptions::default(),
        )
        .expect("adjudicate 3way");

    assert_eq!(
        docket.authors,
        vec!["Dana Feature".to_string()],
        "docket must name the commits' real author, not the @git-author fallback"
    );
}

/// Regression test (X-09): the docket intent/empty notice must be diffed
/// against the merge-base (same as visibility and 3-way analysis), not the
/// base-branch tip. When base tip and head converge to identical trees,
/// diffing the tip against head reports "no file differences" even though
/// merge-base..head clearly changed a file.
#[test]
fn test_git_adapter_docket_diff_uses_merge_base() {
    let repo = TempGitRepo::new("docket_diff_base");

    // Base commit on main
    repo.write_file("src/lib.rs", "pub fn target() -> i32 { 0 }\n");
    repo.commit("base version");

    // Feature branch changes target to 1.
    repo.create_and_checkout_branch("feature/converged");
    repo.write_file("src/lib.rs", "pub fn target() -> i32 { 1 }\n");
    repo.commit("feature change");

    // Main converges on the exact same tree as the feature branch.
    repo.checkout("main");
    repo.write_file("src/lib.rs", "pub fn target() -> i32 { 1 }\n");
    repo.commit("main converged change");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let engine = Engine::new().expect("valid engine");

    let docket = adapter
        .adjudicate_3way(
            "main",
            "feature/converged",
            &engine,
            &MeaningPolicy::default(),
            &VisibilityPolicy::default(),
            &GitAdjudicateOptions::default(),
        )
        .expect("adjudicate 3way");

    // Merge-base..head touches src/lib.rs, so the docket must list it as
    // intent and must not claim there are no file differences. Diffing the
    // base-branch tip against head would (wrongly) produce "no files
    // changed" plus an EMPTY_CHANGE notice for this identical-tree pair.
    assert!(
        !docket.disputes.iter().any(|d| d.id == EMPTY_CHANGE_ID),
        "no EMPTY_CHANGE notice expected: merge-base..head changed files, got disputes: {:?}",
        docket.disputes
    );
    assert_eq!(docket.intent, "src/lib.rs");
}

#[test]
fn test_git_adapter_3way_respects_explicit_authors_override() {
    let repo = TempGitRepo::new("3way_authors_override");

    repo.write_file("src/lib.rs", "pub fn common() -> i32 { 0 }\n");
    repo.commit_as("base version", "Base Author", "base@example.com");

    repo.create_and_checkout_branch("feature/auth-override");
    repo.write_file("src/lib.rs", "pub fn common() -> i32 { 1 }\n");
    repo.commit_as("feature commit", "Commit Author", "commit@example.com");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let engine = Engine::new().expect("valid engine");

    let docket = adapter
        .adjudicate_3way(
            "main",
            "feature/auth-override",
            &engine,
            &MeaningPolicy::default(),
            &VisibilityPolicy::default(),
            &GitAdjudicateOptions {
                authors: Some(vec!["CLI Bob".to_string(), "CLI Alice".to_string()]),
                ..Default::default()
            },
        )
        .expect("adjudicate 3way");

    assert_eq!(
        docket.authors,
        vec!["CLI Alice".to_string(), "CLI Bob".to_string()],
        "explicit --authors override must win over commit-derived authors"
    );
}

#[test]
fn test_git_adapter_authors_bogus_range_errors() {
    let repo = TempGitRepo::new("authors_bogus");
    repo.write_file("src/lib.rs", "pub fn common() -> i32 { 0 }\n");
    repo.commit("base version");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let result = adapter.authors("no-such-ref-xyz");
    assert!(
        result.is_err(),
        "authors() must propagate git failures instead of returning an empty list, got: {:?}",
        result
    );
}

#[test]
fn test_cli_git_adjudication_flags() {
    let repo = TempGitRepo::new("cli_git");
    repo.write_file("src/lib.rs", "pub fn base() {}\n");
    repo.commit("initial base");

    repo.create_and_checkout_branch("feature/cli-test");
    repo.write_file("src/lib.rs", "pub fn base() {}\npub fn extra() {}\n");
    repo.commit("feature commit");

    repo.checkout("main");

    let status = Command::new(env!("CARGO_BIN_EXE_oot"))
        .args([
            "adjudicate",
            "--repo",
            repo.path.to_str().unwrap(),
            "--base-ref",
            "main",
            "--head-ref",
            "feature/cli-test",
            "--change",
            "cli-git-change",
            "--intent",
            "add extra helper",
        ])
        .status()
        .expect("failed to run CLI command");

    assert!(status.success());
}

#[test]
fn test_cli_git_adjudication_respects_authors_flag() {
    let repo = TempGitRepo::new("cli_git_authors");
    repo.write_file("src/lib.rs", "pub fn base() {}\n");
    repo.commit("initial base");

    repo.create_and_checkout_branch("feature/cli-authors");
    repo.write_file("src/lib.rs", "pub fn base() {}\npub fn extra() {}\n");
    repo.commit("feature commit");

    repo.checkout("main");

    let output = Command::new(env!("CARGO_BIN_EXE_oot"))
        .args([
            "adjudicate",
            "--repo",
            repo.path.to_str().unwrap(),
            "--base-ref",
            "main",
            "--head-ref",
            "feature/cli-authors",
            "--authors",
            "CLI Alice,CLI Bob",
        ])
        .output()
        .expect("failed to run CLI command");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("CLI Alice") && stdout.contains("CLI Bob"),
        "VCS adjudication must honor --authors; stdout was: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn test_git_adapter_rejects_non_utf8_path() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo = TempGitRepo::new("non_utf8_path");
    repo.write_file("src/lib.rs", "pub fn ok() {}\n");
    // Invalid UTF-8 in the filename: lossy decoding would collapse distinct
    // byte paths to the same key (SEC-10), so extraction must reject it loudly.
    let bad_name = OsString::from_vec(b"bad\xff.txt".to_vec());
    fs::write(repo.path.join(&bad_name), b"secret").expect("failed to write non-utf8 file");
    let rev = repo.commit("add non-utf8 path");

    let adapter = GitAdapter::new(&repo.path).expect("valid git repo");
    let err = adapter
        .extract_snapshot(&rev)
        .expect_err("non-UTF-8 paths must not be lossily decoded into snapshot keys");
    let msg = err.to_string();
    assert!(
        msg.contains("non-UTF-8 path"),
        "error should identify the non-UTF-8 path, got: {msg}"
    );
}
