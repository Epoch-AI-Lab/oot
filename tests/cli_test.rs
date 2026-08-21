use std::process::Command;

fn get_bin_path() -> String {
    env!("CARGO_BIN_EXE_oot").to_string()
}

#[test]
fn test_cli_adjudicate_fixtures_repo() {
    let bin = get_bin_path();

    let output = Command::new(&bin)
        .args([
            "adjudicate",
            "--change",
            "feature/auth-refactor",
            "--source",
            "jj",
            "--base",
            "fixtures/repo/base",
            "--head",
            "fixtures/repo/head",
            "--authors",
            "@kriday, @agent-7",
            "--visibility",
            "fixtures/visibility.toml",
        ])
        .output()
        .expect("Failed to execute oot CLI");

    // CLOAKED verdict must exit nonzero (do-not-ship).
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("OOT DOCKET"));
    assert!(stdout.contains("change:     feature/auth-refactor"));
    assert!(stdout.contains("from:       jj"));
    assert!(stdout.contains("base:       fixtures/repo/base"));
    assert!(stdout.contains("head:       fixtures/repo/head"));
    assert!(stdout.contains("meaning:    1 disputes detected"));
    assert!(stdout.contains("visibility: 1 private path(s)"));
    assert!(stdout.contains("authors:    @kriday, @agent-7"));
    assert!(stdout.contains("dispute-01: both sides changed `login` (src/lib.rs:1)    [meaning]"));
    assert!(stdout.contains("dispute-02: private path secrets/.env touched by @kriday/@agent-7 (secrets/.env)    [visibility]"));
    assert!(stdout.contains("verdict:    ▶ CLOAKED . 1 requires review, cloaked"));
    assert!(stdout.contains("embargo:    patch held for maintainers until 2026-09-01"));
}

#[test]
fn test_cli_adjudicate_embargoed_clean() {
    let bin = get_bin_path();
    let temp_root = std::env::temp_dir().join(format!("oot_cli_embargo_{}", std::process::id()));
    let base_dir = temp_root.join("base");
    let head_dir = temp_root.join("head");
    let vis_path = temp_root.join("embargo_only.toml");

    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::create_dir_all(&head_dir).unwrap();

    std::fs::write(base_dir.join("main.rs"), "fn foo() {}").unwrap();
    std::fs::write(head_dir.join("main.rs"), "fn foo() { println!(\"1\"); }").unwrap();

    std::fs::write(
        &vis_path,
        "private_paths = []\nembargo_until = \"2026-12-01\"\nprivate_branches = []",
    )
    .unwrap();

    let output = Command::new(&bin)
        .args([
            "adjudicate",
            "--change",
            "security/fix",
            "--source",
            "git",
            "--base",
            base_dir.to_str().unwrap(),
            "--head",
            head_dir.to_str().unwrap(),
            "--visibility",
            vis_path.to_str().unwrap(),
            "--authors",
            "@maintainer",
        ])
        .output()
        .expect("Failed to execute oot CLI");

    // EMBARGOED verdict must exit nonzero (held = do-not-ship).
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("OOT DOCKET"));
    assert!(stdout.contains("verdict:    ▶ EMBARGOED . 1 requires review, held for maintainers"));
    assert!(stdout.contains("embargo:    patch held for maintainers until 2026-12-01"));

    let _ = std::fs::remove_dir_all(&temp_root);
}

#[test]
fn test_cli_adjudicate_load_docket() {
    let bin = get_bin_path();

    let output = Command::new(&bin)
        .args(["adjudicate", "--docket", "fixtures/example.json"])
        .output()
        .expect("Failed to execute oot CLI");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("OOT DOCKET"));
    assert!(stdout.contains("change:     feature/auth-refactor"));
    assert!(stdout.contains("from:       jj"));
    assert!(stdout.contains("base:       main@a3f7c1d"));
    assert!(stdout.contains("head:       feature@b8e2f4a"));
    assert!(stdout.contains("meaning:    1 disputes detected"));
    assert!(stdout.contains("visibility: 1 private path(s)"));
    assert!(stdout.contains("verdict:    ▶ EMBARGOED . 1 requires review, held for maintainers"));
}

#[test]
fn test_cli_custom_meaning_policy_and_temp_dirs() {
    let bin = get_bin_path();
    let temp_root = std::env::temp_dir().join(format!("oot_cli_test_{}", std::process::id()));
    let base_dir = temp_root.join("base");
    let head_dir = temp_root.join("head");
    let policy_path = temp_root.join("strict_policy.toml");

    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::create_dir_all(&head_dir).unwrap();

    // Write base and head rust files
    std::fs::write(
        base_dir.join("main.rs"),
        "fn calculate_total() -> u32 { 100 }",
    )
    .unwrap();
    std::fs::write(
        head_dir.join("main.rs"),
        "fn calculate_total() -> u32 { 200 }",
    )
    .unwrap();

    // Strict policy: review level blocks
    std::fs::write(
        &policy_path,
        "block_on = [\"review\"]\nreview_on = [\"review\"]",
    )
    .unwrap();

    let output = Command::new(&bin)
        .args([
            "adjudicate",
            "--change",
            "patch/calc-update",
            "--source",
            "git",
            "--base",
            base_dir.to_str().unwrap(),
            "--head",
            head_dir.to_str().unwrap(),
            "--policy",
            policy_path.to_str().unwrap(),
            "--authors",
            "@tester",
        ])
        .output()
        .expect("Failed to execute oot CLI");

    // BLOCKED verdict must exit nonzero.
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("OOT DOCKET"));
    assert!(stdout.contains("change:     patch/calc-update"));
    assert!(stdout.contains("meaning:    1 disputes detected"));
    assert!(stdout.contains("verdict:    ▶ BLOCKED . 1 requires review"));

    let _ = std::fs::remove_dir_all(&temp_root);
}

#[test]
fn test_cli_exit_code_zero_for_adjudicated() {
    let bin = get_bin_path();
    let temp_root = std::env::temp_dir().join(format!("oot_cli_clean_{}", std::process::id()));
    let base_dir = temp_root.join("base");
    let head_dir = temp_root.join("head");

    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::create_dir_all(&head_dir).unwrap();

    // Identical snapshots: nothing changed, no disputes, clean verdict.
    std::fs::write(base_dir.join("lib.rs"), "fn ok() {}").unwrap();
    std::fs::write(head_dir.join("lib.rs"), "fn ok() {}").unwrap();

    let output = Command::new(&bin)
        .args([
            "adjudicate",
            "--change",
            "chore/noop",
            "--source",
            "git",
            "--base",
            base_dir.to_str().unwrap(),
            "--head",
            head_dir.to_str().unwrap(),
            "--authors",
            "@tester",
        ])
        .output()
        .expect("Failed to execute oot CLI");

    assert_eq!(
        output.status.code(),
        Some(0),
        "Adjudicated verdict must exit 0"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("OOT DOCKET"));
    assert!(stdout.contains("ADJUDICATED"));

    let _ = std::fs::remove_dir_all(&temp_root);
}

#[test]
fn test_cli_repo_visibility_policy_flags_env() {
    let bin = get_bin_path();

    // The repo's own visibility.toml must flag any .env path as private,
    // producing a CLOAKED verdict and a nonzero exit even for an otherwise
    // empty diff.
    let output = Command::new(&bin)
        .args([
            "adjudicate",
            "--change",
            "test/policy-check",
            "--source",
            "git",
            "--base",
            "fixtures/repo/base",
            "--head",
            "fixtures/repo/head",
            "--visibility",
            "visibility.toml",
            "--authors",
            "@tester",
        ])
        .output()
        .expect("Failed to execute oot CLI");

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("CLOAKED"));
    assert!(stdout.contains(".env"));

    let _ = ();
}

#[test]
fn test_cli_missing_base_and_docket_fails() {
    let bin = get_bin_path();

    let output = Command::new(&bin)
        .args(["adjudicate"])
        .output()
        .expect("Failed to execute oot CLI");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("provide --docket <file>"));
}

#[test]
fn test_cli_missing_head_fails() {
    let bin = get_bin_path();

    let output = Command::new(&bin)
        .args(["adjudicate", "--base", "fixtures/repo/base"])
        .output()
        .expect("Failed to execute oot CLI");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("provide --docket <file>"));
}

#[test]
fn test_cli_invalid_source_fails() {
    let bin = get_bin_path();

    let output = Command::new(&bin)
        .args([
            "adjudicate",
            "--source",
            "invalid_vcs",
            "--base",
            "fixtures/repo/base",
            "--head",
            "fixtures/repo/head",
        ])
        .output()
        .expect("Failed to execute oot CLI");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown source: invalid_vcs"));
}
