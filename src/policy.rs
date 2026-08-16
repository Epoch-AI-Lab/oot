use crate::dispute::{Dispute, Kind, Severity, Verdict};
use serde::Deserialize;
use std::path::Path;

/// Thresholds for *meaning* disputes. Visibility has its own policy.
#[derive(Debug, Deserialize)]
pub struct MeaningPolicy {
    /// Severity names (lowercase) that block the change.
    pub block_on: Vec<String>,
    /// Severity names (lowercase) that require a human to review.
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
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let p: MeaningPolicy = toml::from_str(&text)?;
        Ok(p)
    }

    /// Evaluate only meaning disputes. Visibility disputes are judged elsewhere.
    pub fn evaluate(&self, disputes: &[Dispute]) -> Verdict {
        for d in disputes {
            if d.kind != Kind::Meaning {
                continue;
            }
            let s = d.severity.as_str();
            if self.block_on.iter().any(|b| b.eq_ignore_ascii_case(s)) {
                return Verdict::Blocked;
            }
        }
        Verdict::Adjudicated
    }

    pub fn requires_review(&self, disputes: &[Dispute]) -> bool {
        disputes.iter().any(|d| {
            d.kind == Kind::Meaning
                && self
                    .review_on
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(d.severity.as_str()))
        })
    }
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Review => "review",
            Severity::High => "high",
        }
    }
}
