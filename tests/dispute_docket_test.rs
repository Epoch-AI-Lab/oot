use oot::dispute::{Dispute, Docket, Kind, Severity, Verdict};
use oot::docket;
use std::path::Path;

fn sample_docket() -> Docket {
    Docket {
        change: "feature/auth-layer".into(),
        source: "jj".into(),
        base: "main@commit1".into(),
        head: "feature@commit2".into(),
        disputes: vec![
            Dispute {
                id: "D001".into(),
                location: "src/auth.rs:15".into(),
                kind: Kind::Meaning,
                severity: Severity::Review,
                detail: "both sides changed `verify_token`".into(),
            },
            Dispute {
                id: "D002".into(),
                location: "src/auth.rs:30".into(),
                kind: Kind::Meaning,
                severity: Severity::Low,
                detail: "added function `refresh_token`".into(),
            },
            Dispute {
                id: "V001".into(),
                location: "secrets/.env".into(),
                kind: Kind::Visibility,
                severity: Severity::High,
                detail: "private path secrets/.env touched by @alice/@bob".into(),
            },
        ],
        intent: "Authentication and session management".into(),
        authors: vec!["@alice".into(), "@bob".into()],
        verdict: Verdict::Embargoed,
        embargo: Some("patch held for maintainers until 2026-09-01".into()),
    }
}

#[test]
fn test_dispute_classification_and_counts() {
    let docket = sample_docket();

    assert_eq!(docket.meaning_count(), 2);
    assert_eq!(docket.visibility_count(), 1);
    assert_eq!(docket.review_count(), 1);
    assert!(docket.requires_review());

    let clean_docket = Docket {
        change: "docs".into(),
        source: "git".into(),
        base: "main".into(),
        head: "docs".into(),
        disputes: vec![],
        intent: "doc updates".into(),
        authors: vec!["@writer".into()],
        verdict: Verdict::Adjudicated,
        embargo: None,
    };

    assert_eq!(clean_docket.meaning_count(), 0);
    assert_eq!(clean_docket.visibility_count(), 0);
    assert_eq!(clean_docket.review_count(), 0);
    assert!(!clean_docket.requires_review());
}

#[test]
fn test_docket_json_serialization_roundtrip() {
    let original = sample_docket();

    let json_str = docket::to_json(&original).expect("JSON serialization failed");
    let deserialized = docket::from_json(&json_str).expect("JSON deserialization failed");

    assert_eq!(deserialized.change, original.change);
    assert_eq!(deserialized.source, original.source);
    assert_eq!(deserialized.base, original.base);
    assert_eq!(deserialized.head, original.head);
    assert_eq!(deserialized.disputes.len(), original.disputes.len());
    assert_eq!(deserialized.intent, original.intent);
    assert_eq!(deserialized.authors, original.authors);
    assert_eq!(deserialized.verdict, original.verdict);
    assert_eq!(deserialized.embargo, original.embargo);
}

#[test]
fn test_docket_toml_serialization_roundtrip() {
    let original = sample_docket();

    let toml_str = docket::to_toml(&original).expect("TOML serialization failed");
    let deserialized = docket::from_toml(&toml_str).expect("TOML deserialization failed");

    assert_eq!(deserialized.change, original.change);
    assert_eq!(deserialized.source, original.source);
    assert_eq!(deserialized.base, original.base);
    assert_eq!(deserialized.head, original.head);
    assert_eq!(deserialized.disputes.len(), original.disputes.len());
    assert_eq!(deserialized.intent, original.intent);
    assert_eq!(deserialized.authors, original.authors);
    assert_eq!(deserialized.verdict, original.verdict);
    assert_eq!(deserialized.embargo, original.embargo);
}

#[test]
fn test_docket_save_and_load_json_file() {
    let original = sample_docket();
    let temp_dir = std::env::temp_dir().join(format!("oot_docket_test_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let file_path = temp_dir.join("test_docket.json");

    docket::save_json(&original, &file_path).expect("save_json failed");
    let loaded = docket::load_json(&file_path).expect("load_json failed");

    assert_eq!(loaded.change, original.change);
    assert_eq!(loaded.verdict, original.verdict);

    let generic_loaded = docket::load(&file_path).expect("generic load failed");
    assert_eq!(generic_loaded.change, original.change);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_docket_save_and_load_toml_file() {
    let original = sample_docket();
    let temp_dir =
        std::env::temp_dir().join(format!("oot_docket_test_toml_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let file_path = temp_dir.join("test_docket.toml");

    docket::save_toml(&original, &file_path).expect("save_toml failed");
    let loaded = docket::load_toml(&file_path).expect("load_toml failed");

    assert_eq!(loaded.change, original.change);
    assert_eq!(loaded.verdict, original.verdict);

    let generic_loaded = docket::load(&file_path).expect("generic load failed");
    assert_eq!(generic_loaded.change, original.change);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_docket_load_from_example_fixture() {
    let fixture_path = Path::new("fixtures/example.json");
    let docket = docket::load(fixture_path).expect("Failed to load fixtures/example.json");

    assert_eq!(docket.change, "feature/auth-refactor");
    assert_eq!(docket.source, "jj");
    assert_eq!(docket.base, "main@a3f7c1d");
    assert_eq!(docket.head, "feature@b8e2f4a");
    assert_eq!(docket.disputes.len(), 2);
    assert_eq!(docket.meaning_count(), 1);
    assert_eq!(docket.visibility_count(), 1);
    assert_eq!(docket.authors, vec!["@kriday", "@agent-7"]);
    assert_eq!(docket.verdict, Verdict::Embargoed);
    assert_eq!(
        docket.embargo.as_deref(),
        Some("patch held for maintainers until 2026-09-01")
    );
}

#[test]
fn test_docket_render_verdicts() {
    // 1. Cloaked verdict
    let mut cloaked = sample_docket();
    cloaked.verdict = Verdict::Cloaked;
    let rendered_cloaked = cloaked.render();
    assert!(rendered_cloaked.contains("verdict:    ▶ CLOAKED . 1 requires review, cloaked"));

    // 2. Blocked verdict
    let mut blocked = sample_docket();
    blocked.verdict = Verdict::Blocked;
    let rendered_blocked = blocked.render();
    assert!(rendered_blocked.contains("verdict:    ▶ BLOCKED . 1 requires review"));

    // 3. Adjudicated verdict with no reviews
    let mut adjudicated = sample_docket();
    adjudicated.disputes.clear();
    adjudicated.verdict = Verdict::Adjudicated;
    let rendered_adjudicated = adjudicated.render();
    assert!(rendered_adjudicated.contains("verdict:    ▶ ADJUDICATED \n"));
    assert!(rendered_adjudicated.contains("dispute:    none"));
}
