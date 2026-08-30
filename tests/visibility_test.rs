use oot::change::{Change, Snapshot, Source};
use oot::dispute::{Kind, Severity};
use oot::visibility::VisibilityPolicy;
use std::path::Path;

#[test]
fn test_visibility_policy_default_values() {
    let policy = VisibilityPolicy::default();
    assert_eq!(policy.private_paths, vec!["secrets/", ".env"]);
    assert_eq!(policy.embargo_until, None);
    assert!(policy.private_branches.is_empty());
    assert_eq!(policy.embargo_note(), None);
}

#[test]
fn test_visibility_policy_deserialization_from_fixture() {
    let fixture_path = Path::new("fixtures/visibility.toml");
    let policy =
        VisibilityPolicy::load(fixture_path).expect("Failed to load fixtures/visibility.toml");

    assert_eq!(policy.private_paths, vec!["secrets/", ".env"]);
    assert_eq!(policy.embargo_until.as_deref(), Some("2026-09-01"));
    assert!(policy.private_branches.is_empty());
    assert_eq!(
        policy.embargo_note().as_deref(),
        Some("patch held for maintainers until 2026-09-01")
    );
}

#[test]
fn test_visibility_policy_private_paths_detection() {
    let policy = VisibilityPolicy {
        private_paths: vec!["secrets/".into(), ".env".into(), "credentials.json".into()],
        embargo_until: None,
        private_branches: vec![],
    };

    let mut head = Snapshot::default();
    head.files
        .insert("src/main.rs".into(), "fn main() {}".into());
    head.files
        .insert("secrets/db_password.txt".into(), "pass123".into());
    head.files
        .insert("config/.env.local".into(), "SECRET=foo".into());

    let change = Change {
        name: "feature/add-config".into(),
        source: Source::Git,
        base_ref: "main".into(),
        head_ref: "feature/add-config".into(),
        base: Snapshot::default(),
        head,
        authors: vec!["@alice".into(), "@bob".into()],
        intent: Some("Added local secrets".into()),
    };

    let disputes = policy.check(&change);

    // 2 private paths touched: secrets/db_password.txt and config/.env.local
    assert_eq!(disputes.len(), 2);
    for d in &disputes {
        assert_eq!(d.kind, Kind::Visibility);
        assert_eq!(d.severity, Severity::High);
        assert!(d.detail.contains("@alice/@bob"));
    }

    let locations: Vec<&str> = disputes.iter().map(|d| d.location.as_str()).collect();
    assert!(locations.contains(&"secrets/db_password.txt"));
    assert!(locations.contains(&"config/.env.local"));
}

#[test]
fn test_visibility_policy_private_branch_matching() {
    let policy = VisibilityPolicy {
        private_paths: vec![],
        embargo_until: None,
        private_branches: vec!["confidential-fix".into(), "security-audit".into()],
    };

    // Change matching private branch in change name
    let change_1 = Change {
        name: "feature/confidential-fix-v1".into(),
        source: Source::Git,
        base_ref: "main".into(),
        head_ref: "feature/confidential-fix-v1".into(),
        base: Snapshot::default(),
        head: Snapshot::default(),
        authors: vec!["@secops".into()],
        intent: None,
    };

    let disputes_1 = policy.check(&change_1);
    assert_eq!(disputes_1.len(), 1);
    assert_eq!(disputes_1[0].kind, Kind::Visibility);
    assert_eq!(disputes_1[0].severity, Severity::High);
    assert!(disputes_1[0]
        .detail
        .contains("private branch confidential-fix referenced by @secops"));

    // Change matching private branch in head_ref
    let change_2 = Change {
        name: "unnamed-pr".into(),
        source: Source::Jj,
        base_ref: "main".into(),
        head_ref: "refs/heads/security-audit-branch".into(),
        base: Snapshot::default(),
        head: Snapshot::default(),
        authors: vec!["@secops".into()],
        intent: None,
    };

    let disputes_2 = policy.check(&change_2);
    assert_eq!(disputes_2.len(), 1);
    assert!(disputes_2[0]
        .detail
        .contains("private branch security-audit referenced by @secops"));

    // Change not matching any private branch
    let clean_change = Change {
        name: "feature/public-ui".into(),
        source: Source::Git,
        base_ref: "main".into(),
        head_ref: "feature/public-ui".into(),
        base: Snapshot::default(),
        head: Snapshot::default(),
        authors: vec!["@frontend".into()],
        intent: None,
    };

    let clean_disputes = policy.check(&clean_change);
    assert!(clean_disputes.is_empty());
}

#[test]
fn test_visibility_policy_embargo_formatting() {
    let mut policy = VisibilityPolicy::default();
    assert_eq!(policy.embargo_note(), None);

    policy.embargo_until = Some("2026-11-15".into());
    assert_eq!(
        policy.embargo_note().as_deref(),
        Some("patch held for maintainers until 2026-11-15")
    );
}

#[test]
fn test_visibility_policy_slash_stripping_and_matching() {
    let policy = VisibilityPolicy {
        private_paths: vec!["/internal/keys/".into(), "/cert.pem".into()],
        embargo_until: None,
        private_branches: vec![],
    };

    let mut head = Snapshot::default();
    head.files
        .insert("nested/internal/keys/private.key".into(), "KEY".into());
    head.files.insert("cert.pem".into(), "CERT".into());

    let change = Change {
        name: "infra-update".into(),
        source: Source::Memory,
        base_ref: "base".into(),
        head_ref: "head".into(),
        base: Snapshot::default(),
        head,
        authors: vec!["@infra".into()],
        intent: None,
    };

    let disputes = policy.check(&change);
    assert_eq!(disputes.len(), 2);
}

#[test]
fn test_visibility_policy_detects_deleted_private_paths() {
    let policy = VisibilityPolicy {
        private_paths: vec!["secrets/".into(), ".env".into()],
        embargo_until: None,
        private_branches: vec![],
    };

    let mut base = Snapshot::default();
    base.files
        .insert("secrets/api_token.txt".into(), "secret123".into());
    base.files
        .insert("src/lib.rs".into(), "pub fn run() {}".into());

    let mut head = Snapshot::default();
    head.files
        .insert("src/lib.rs".into(), "pub fn run() {}".into());
    // secrets/api_token.txt was deleted in head

    let change = Change {
        name: "pr-delete-secret".into(),
        source: Source::Git,
        base_ref: "main".into(),
        head_ref: "pr-delete-secret".into(),
        base,
        head,
        authors: vec!["@contributor".into()],
        intent: Some("Cleanup".into()),
    };

    let disputes = policy.check(&change);
    assert_eq!(disputes.len(), 1);
    assert_eq!(disputes[0].location, "secrets/api_token.txt");
    assert_eq!(disputes[0].kind, Kind::Visibility);
}
