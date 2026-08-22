//! Meaning policy configuration and dispute evaluation.
//!
//! Controls thresholds for semantic and structural disputes.

use crate::dispute::{Dispute, Kind, Verdict};
use serde::Deserialize;
use std::path::Path;

/// Thresholds for *meaning* disputes. Visibility has its own policy.
#[derive(Debug, Deserialize)]
pub struct MeaningPolicy {
    /// Severity names (lowercase, e.g. `"high"`) that block the change.
    pub block_on: Vec<String>,
    /// Severity names (lowercase, e.g. `"review"`, `"high"`) that require human review.
    pub review_on: Vec<String>,
}

impl Default for MeaningPolicy {
    fn default() -> Self {
        MeaningPolicy {
            block_on: vec!["high".into()],
            review_on: vec!["review".into(), "high".into()],
        }
    }
}

impl MeaningPolicy {
    /// Load a meaning policy from a TOML configuration file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let p: MeaningPolicy = toml::from_str(&text)?;
        Ok(p)
    }

    /// Evaluate only meaning disputes against blocking thresholds.
    ///
    /// The empty-change notice (`EMPTY_CHANGE_ID`) never blocks: it is
    /// informational and excluded here so a saved docket re-evaluated under
    /// any policy behaves the same as at adjudication time.
    pub fn evaluate(&self, disputes: &[Dispute]) -> Verdict {
        for d in disputes {
            if d.kind != Kind::Meaning || d.id == crate::dispute::EMPTY_CHANGE_ID {
                continue;
            }
            let s = d.severity.as_str();
            if self.block_on.iter().any(|b| b.eq_ignore_ascii_case(s)) {
                return Verdict::Blocked;
            }
        }
        Verdict::Adjudicated
    }

    /// Check if any meaning dispute requires human review based on `review_on` rules.
    pub fn requires_review(&self, disputes: &[Dispute]) -> bool {
        disputes.iter().any(|d| {
            d.kind == Kind::Meaning
                && self
                    .review_on
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(d.severity.as_str()))
        })
    }

    /// Count how many meaning disputes require human review based on `review_on` rules.
    pub fn review_count(&self, disputes: &[Dispute]) -> usize {
        disputes
            .iter()
            .filter(|d| {
                d.kind == Kind::Meaning
                    && self
                        .review_on
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(d.severity.as_str()))
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispute::Severity;

    #[test]
    fn test_meaning_policy_default_evaluation() {
        let policy = MeaningPolicy::default();

        let disputes = vec![Dispute {
            id: "D001".into(),
            location: "src/lib.rs:10".into(),
            kind: Kind::Meaning,
            severity: Severity::Review,
            detail: "changed signature".into(),
        }];

        assert_eq!(policy.evaluate(&disputes), Verdict::Adjudicated);
        assert!(policy.requires_review(&disputes));
        assert_eq!(policy.review_count(&disputes), 1);
    }

    #[test]
    fn test_meaning_policy_blocked() {
        let policy = MeaningPolicy::default();

        let disputes = vec![Dispute {
            id: "D002".into(),
            location: "src/lib.rs:20".into(),
            kind: Kind::Meaning,
            severity: Severity::High,
            detail: "breaking change".into(),
        }];

        assert_eq!(policy.evaluate(&disputes), Verdict::Blocked);
        assert!(policy.requires_review(&disputes));
        assert_eq!(policy.review_count(&disputes), 1);
    }

    #[test]
    fn test_meaning_policy_low_severity_and_visibility_ignore() {
        let policy = MeaningPolicy::default();

        let disputes = vec![
            Dispute {
                id: "D003".into(),
                location: "src/lib.rs:30".into(),
                kind: Kind::Meaning,
                severity: Severity::Low,
                detail: "minor format change".into(),
            },
            Dispute {
                id: "V001".into(),
                location: ".env".into(),
                kind: Kind::Visibility,
                severity: Severity::High,
                detail: "private path touched".into(),
            },
        ];

        // Low meaning dispute doesn't block and doesn't require review
        // Visibility dispute is ignored by MeaningPolicy
        assert_eq!(policy.evaluate(&disputes), Verdict::Adjudicated);
        assert!(!policy.requires_review(&disputes));
        assert_eq!(policy.review_count(&disputes), 0);
    }

    #[test]
    fn test_meaning_policy_custom_toml() {
        let toml_content = r#"
            block_on = ["review", "high"]
            review_on = ["low"]
        "#;
        let policy: MeaningPolicy = toml::from_str(toml_content).unwrap();

        assert_eq!(policy.block_on, vec!["review", "high"]);
        assert_eq!(policy.review_on, vec!["low"]);

        let review_dispute = vec![Dispute {
            id: "D004".into(),
            location: "src/lib.rs:5".into(),
            kind: Kind::Meaning,
            severity: Severity::Review,
            detail: "review level".into(),
        }];

        // Under custom policy, review level blocks
        assert_eq!(policy.evaluate(&review_dispute), Verdict::Blocked);
        assert!(!policy.requires_review(&review_dispute));
    }
}
