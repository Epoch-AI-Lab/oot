use oot::dispute::{Dispute, Kind, Severity, Verdict};
use oot::policy::MeaningPolicy;
use std::io::Write;

fn make_dispute(id: &str, kind: Kind, severity: Severity, detail: &str) -> Dispute {
    Dispute {
        id: id.to_string(),
        location: "src/engine.rs:42".to_string(),
        kind,
        severity,
        detail: detail.to_string(),
    }
}

#[test]
fn test_meaning_policy_default_fallbacks() {
    let policy = MeaningPolicy::default();

    assert_eq!(policy.block_on, vec!["high"]);
    assert_eq!(policy.review_on, vec!["review", "high"]);

    // Test with no disputes
    let empty_disputes: Vec<Dispute> = vec![];
    assert_eq!(policy.evaluate(&empty_disputes), Verdict::Adjudicated);
    assert!(!policy.requires_review(&empty_disputes));
    assert_eq!(policy.review_count(&empty_disputes), 0);

    // Test with Low severity meaning dispute
    let low_disputes = vec![make_dispute(
        "D001",
        Kind::Meaning,
        Severity::Low,
        "low severity",
    )];
    assert_eq!(policy.evaluate(&low_disputes), Verdict::Adjudicated);
    assert!(!policy.requires_review(&low_disputes));
    assert_eq!(policy.review_count(&low_disputes), 0);

    // Test with Review severity meaning dispute
    let review_disputes = vec![make_dispute(
        "D002",
        Kind::Meaning,
        Severity::Review,
        "review severity",
    )];
    assert_eq!(policy.evaluate(&review_disputes), Verdict::Adjudicated);
    assert!(policy.requires_review(&review_disputes));
    assert_eq!(policy.review_count(&review_disputes), 1);

    // Test with High severity meaning dispute
    let high_disputes = vec![make_dispute(
        "D003",
        Kind::Meaning,
        Severity::High,
        "high severity",
    )];
    assert_eq!(policy.evaluate(&high_disputes), Verdict::Blocked);
    assert!(policy.requires_review(&high_disputes));
    assert_eq!(policy.review_count(&high_disputes), 1);
}

#[test]
fn test_meaning_policy_toml_deserialization_and_file_load() {
    let toml_str = r#"
        block_on = ["high", "review"]
        review_on = ["low", "review"]
    "#;

    let policy: MeaningPolicy =
        toml::from_str(toml_str).expect("Failed to deserialize TOML string");
    assert_eq!(policy.block_on, vec!["high", "review"]);
    assert_eq!(policy.review_on, vec!["low", "review"]);

    // Write to a temporary file and test MeaningPolicy::load()
    let mut temp_file = tempfile_named("meaning_policy.toml");
    temp_file
        .write_all(toml_str.as_bytes())
        .expect("Failed to write temp toml file");

    let loaded_policy =
        MeaningPolicy::load(temp_file.path()).expect("Failed to load MeaningPolicy from file");
    assert_eq!(loaded_policy.block_on, vec!["high", "review"]);
    assert_eq!(loaded_policy.review_on, vec!["low", "review"]);

    let low_dispute = vec![make_dispute(
        "D001",
        Kind::Meaning,
        Severity::Low,
        "low dispute",
    )];
    // Under this custom policy, low requires review
    assert!(loaded_policy.requires_review(&low_dispute));
    assert_eq!(loaded_policy.evaluate(&low_dispute), Verdict::Adjudicated);

    let review_dispute = vec![make_dispute(
        "D002",
        Kind::Meaning,
        Severity::Review,
        "review dispute",
    )];
    // Under this custom policy, review blocks
    assert_eq!(loaded_policy.evaluate(&review_dispute), Verdict::Blocked);
}

#[test]
fn test_meaning_policy_case_insensitivity() {
    let toml_str = r#"
        block_on = ["HIGH", "Review"]
        review_on = ["LoW", "REVIEW"]
    "#;

    let policy: MeaningPolicy = toml::from_str(toml_str).expect("Failed to deserialize TOML");

    let high_dispute = vec![make_dispute("D001", Kind::Meaning, Severity::High, "high")];
    assert_eq!(policy.evaluate(&high_dispute), Verdict::Blocked);

    let low_dispute = vec![make_dispute("D002", Kind::Meaning, Severity::Low, "low")];
    assert!(policy.requires_review(&low_dispute));
}

#[test]
fn test_meaning_policy_ignores_visibility_disputes() {
    let policy = MeaningPolicy::default();

    let visibility_disputes = vec![
        make_dispute(
            "V001",
            Kind::Visibility,
            Severity::High,
            "private path secrets/.env touched",
        ),
        make_dispute(
            "V002",
            Kind::Visibility,
            Severity::Review,
            "private branch referenced",
        ),
    ];

    // Meaning policy should not block or trigger review on visibility disputes
    assert_eq!(policy.evaluate(&visibility_disputes), Verdict::Adjudicated);
    assert!(!policy.requires_review(&visibility_disputes));
    assert_eq!(policy.review_count(&visibility_disputes), 0);
}

#[test]
fn test_meaning_policy_mixed_disputes() {
    let policy = MeaningPolicy::default();

    let disputes = vec![
        make_dispute("D001", Kind::Meaning, Severity::Low, "minor refactor"),
        make_dispute(
            "D002",
            Kind::Meaning,
            Severity::Review,
            "changed public API",
        ),
        make_dispute("V001", Kind::Visibility, Severity::High, "touched .env"),
    ];

    assert_eq!(policy.evaluate(&disputes), Verdict::Adjudicated);
    assert!(policy.requires_review(&disputes));
    assert_eq!(policy.review_count(&disputes), 1);
}

fn tempfile_named(name: &str) -> TempFileGuard {
    let dir = std::env::temp_dir().join(format!("oot_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    TempFileGuard { path }
}

struct TempFileGuard {
    path: std::path::PathBuf,
}

impl TempFileGuard {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl std::io::Write for TempFileGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
