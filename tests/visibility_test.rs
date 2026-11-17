use oot::change::{Change, Snapshot, Source};
use oot::dispute::{Kind, Severity};
use oot::visibility::{parse_date_ymd, VisibilityPolicy};
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
    assert_eq!(policy.embargo_until.as_deref(), Some("2027-09-01"));
    assert!(policy.private_branches.is_empty());
    assert_eq!(
        policy.embargo_note().as_deref(),
        Some("patch held for maintainers until 2027-09-01")
    );
}

#[test]
fn test_visibility_policy_private_paths_detection() {
    let policy = VisibilityPolicy {
        private_paths: vec!["secrets/".into(), ".env".into(), "credentials.json".into()],
        embargo_until: None,
        private_branches: vec![],
        resign_key_id: None,
        embargo_recipients: vec![],
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
        resign_key_id: None,
        embargo_recipients: vec![],
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
        resign_key_id: None,
        embargo_recipients: vec![],
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
        resign_key_id: None,
        embargo_recipients: vec![],
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

#[test]
fn test_visibility_policy_dotfile_root_and_nested_exact_matching() {
    let policy = VisibilityPolicy {
        private_paths: vec![".env".into(), "secrets/".into()],
        embargo_until: None,
        private_branches: vec!["private/*".into()],
        resign_key_id: None,
        embargo_recipients: vec![],
    };

    // Root-level and nested .env variants MUST be private
    assert!(policy.path_is_private(".env"));
    assert!(policy.path_is_private(".env.local"));
    assert!(policy.path_is_private(".env.production"));
    assert!(policy.path_is_private("config/.env.staging"));
    assert!(policy.path_is_private("secrets/key.pem"));

    // Similar names that are NOT .env variants or secrets directory MUST NOT be private
    assert!(!policy.path_is_private(".environment.rs"));
    assert!(!policy.path_is_private("config/.envoy.yaml"));
    assert!(!policy.path_is_private("src/keyboard.rs"));
    assert!(!policy.path_is_private("src/secrets_manager.rs"));
}

#[test]
fn test_embargo_date_formats_and_validation() {
    let policy_iso = VisibilityPolicy {
        private_paths: vec![],
        embargo_until: Some("2099-01-01".into()),
        private_branches: vec![],
        resign_key_id: None,
        embargo_recipients: vec![],
    };
    assert!(
        policy_iso.is_under_embargo(),
        "future ISO date must be under embargo"
    );

    let policy_dd_mm_yyyy = VisibilityPolicy {
        private_paths: vec![],
        embargo_until: Some("01-01-2099".into()),
        private_branches: vec![],
        resign_key_id: None,
        embargo_recipients: vec![],
    };
    assert!(
        policy_dd_mm_yyyy.is_under_embargo(),
        "future DD-MM-YYYY date must be under embargo"
    );

    let policy_slash = VisibilityPolicy {
        private_paths: vec![],
        embargo_until: Some("2099/12/31".into()),
        private_branches: vec![],
        resign_key_id: None,
        embargo_recipients: vec![],
    };
    assert!(
        policy_slash.is_under_embargo(),
        "future slash date must be under embargo"
    );

    let policy_past = VisibilityPolicy {
        private_paths: vec![],
        embargo_until: Some("1999-01-01".into()),
        private_branches: vec![],
        resign_key_id: None,
        embargo_recipients: vec![],
    };
    assert!(
        !policy_past.is_under_embargo(),
        "past date must not be under embargo"
    );

    // Invalid format loading must fail
    let tmp = std::env::temp_dir().join(format!("oot-bad-date-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    for bad in [
        "not-a-date",
        "2099-02-30",
        "2099-13-01",
        "2099-12/31",
        "01-01-2099-extra",
    ] {
        let bad_toml = tmp.join("bad.toml");
        std::fs::write(&bad_toml, format!("embargo_until = \"{bad}\"\n")).unwrap();
        assert!(
            VisibilityPolicy::load(&bad_toml).is_err(),
            "invalid date '{bad}' in TOML must fail to load"
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);

    // Leap day exists in 2096 but not in 2099.
    assert!(VisibilityPolicy {
        private_paths: vec![],
        embargo_until: Some("2096-02-29".into()),
        private_branches: vec![],
        resign_key_id: None,
        embargo_recipients: vec![],
    }
    .is_under_embargo());
    // Nonexistent Feb 29 cannot be constructed, so a hand-built policy
    // with it must fail closed, never open.
    assert!(VisibilityPolicy {
        private_paths: vec![],
        embargo_until: Some("2099-02-29".into()),
        private_branches: vec![],
        resign_key_id: None,
        embargo_recipients: vec![],
    }
    .is_under_embargo());
}

#[test]
fn test_embargo_date_rejects_wrapping_components() {
    // 2^32 + 1 used to wrap to 1 in the ISO day slot and get accepted.
    assert_eq!(parse_date_ymd("2026-02-4294967297"), None);
    // The same value in the day-first year slot used to load as an absurd year.
    assert_eq!(parse_date_ymd("1-02-4294967297"), None);

    let tmp = std::env::temp_dir().join(format!("oot-wrap-date-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    for bad in ["2026-02-4294967297", "1-02-4294967297"] {
        let path = tmp.join("bad.toml");
        std::fs::write(&path, format!("embargo_until = \"{bad}\"\n")).unwrap();
        let err = VisibilityPolicy::load(&path).expect_err("wrapping date must fail to load");
        assert!(
            err.to_string().contains("invalid embargo_until"),
            "expected invalid embargo_until error for '{bad}', got: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);

    // Valid dates in both forms, including a leap day, still parse.
    assert_eq!(parse_date_ymd("2024-02-29"), Some((2024, 2, 29)));
    assert_eq!(parse_date_ymd("01-03-2099"), Some((2099, 3, 1)));
}

// Regression (V16): `\u{1f}` is the canonical list separator. An item that
// contains it used to collide with two separate items, giving both policies
// the same audit_key. Escaping the separator inside items must break the tie.
#[test]
fn test_visibility_fingerprint_separator_collision_is_injective() {
    let dir = std::env::temp_dir().join(format!(
        "oot_visibility_sep_collision_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let two_items = dir.join("two_items.toml");
    let one_embedded_item = dir.join("one_embedded_item.toml");
    std::fs::write(&two_items, "private_paths = [\"a\", \"b\"]\n").unwrap();
    // TOML \u001F escape loads the unit separator into a single item.
    std::fs::write(&one_embedded_item, "private_paths = [\"a\\u001Fb\"]\n").unwrap();

    let a = VisibilityPolicy::load(&two_items).expect("two-item policy must load");
    let b =
        VisibilityPolicy::load(&one_embedded_item).expect("embedded-separator policy must load");
    assert_eq!(a.private_paths, vec!["a", "b"]);
    assert_eq!(b.private_paths, vec!["a\u{1f}b"]);

    eprintln!("two-item canonical: {:?}", a.canonical_fields());
    eprintln!("two-item audit_key: {}", a.audit_key());
    eprintln!("embedded canonical: {:?}", b.canonical_fields());
    eprintln!("embedded audit_key: {}", b.audit_key());

    assert_ne!(a.canonical_fields(), b.canonical_fields());
    assert_ne!(a.audit_key(), b.audit_key());

    let _ = std::fs::remove_dir_all(&dir);
}

// Ordinary policies keep their old fingerprint. This pins the canonical
// encoding so a future change cannot silently move every audit_key (and wipe
// every export cache) without someone updating these expected values.
#[test]
fn test_visibility_fingerprint_stable_for_ordinary_lists() {
    let default_policy = VisibilityPolicy::default();
    assert_eq!(
        default_policy.canonical_fields(),
        "embargo_until=;resign_key_id=;private_paths=.env\u{1f}secrets/\u{1f};private_branches=;embargo_recipients="
    );
    assert_eq!(default_policy.audit_key(), "95b1eab997115f3f");

    let full = VisibilityPolicy {
        private_paths: vec!["secrets/".into(), ".env".into()],
        embargo_until: Some("2027-09-01".into()),
        private_branches: vec!["internal-audit".into()],
        resign_key_id: Some("KEY1".into()),
        embargo_recipients: vec!["alice@example.com".into()],
    };
    assert_eq!(full.audit_key(), "ab0bf5b5d3376f1e");
}
