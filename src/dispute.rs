//! Dispute models and docket adjudication records.
//!
//! A [`Dispute`] represents a point of disagreement (either structural meaning
//! or visibility violation). A [`Docket`] is the complete, rendered adjudication
//! record containing disputes, verdict, intent, and embargo metadata.

use serde::{Deserialize, Serialize};

/// The category of dispute raised during adjudication.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Semantic or structural code dispute (e.g. diverging function implementation).
    Meaning,
    /// Governance or visibility dispute (e.g. private file touched, private branch referenced).
    Visibility,
}

/// The severity level of a dispute.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Low severity notice; informative only.
    Low,
    /// Requires explicit review by human maintainers.
    Review,
    /// High severity violation; blocks automated acceptance or cloaks the change.
    High,
}

impl Severity {
    /// Return the canonical lowercase string for this severity level.
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Review => "review",
            Severity::High => "high",
        }
    }
}

/// An individual dispute identified between snapshots or policies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dispute {
    /// Unique identifier for the dispute (e.g. `"D001"`, `"V001"`).
    pub id: String,
    /// File path and optional row/line location (e.g. `"src/lib.rs:42"`).
    pub location: String,
    /// Category of dispute: meaning or visibility.
    pub kind: Kind,
    /// Assessed severity level.
    pub severity: Severity,
    /// Human-readable explanation of the dispute.
    pub detail: String,
}

impl Dispute {
    /// Low-severity notice that a change contains no file differences.
    ///
    /// Does not affect blocking or review thresholds; it only makes the
    /// empty change visible on the docket instead of passing silently.
    pub fn empty_change() -> Dispute {
        Dispute {
            id: "D000".into(),
            location: "-".into(),
            kind: Kind::Meaning,
            severity: Severity::Low,
            detail: "no file differences between base and head".into(),
        }
    }
}

/// The final adjudication verdict for a change.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Change passed checks or is within acceptable review thresholds.
    Adjudicated,
    /// Change was blocked due to severe meaning disputes.
    Blocked,
    /// Change is held under an active embargo schedule.
    Embargoed,
    /// Change touched private/restricted paths or branches and must be cloaked.
    Cloaked,
}

/// The full adjudication record for one Change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Docket {
    /// Identifier or name of the change.
    pub change: String,
    /// Source environment (git, jj, memory).
    pub source: String,
    /// Base reference or directory.
    pub base: String,
    /// Head reference or directory.
    pub head: String,
    /// Collection of detected disputes.
    pub disputes: Vec<Dispute>,
    /// Stated intent of the change, or a summary of touched paths when none was given.
    #[serde(alias = "scope")]
    pub intent: String,
    /// Change author handles or agent identifiers.
    pub authors: Vec<String>,
    /// Resulting adjudication verdict.
    pub verdict: Verdict,
    /// Embargo notice string, if an embargo is in effect.
    pub embargo: Option<String>,
}

impl Docket {
    /// Return the number of meaning-related disputes.
    pub fn meaning_count(&self) -> usize {
        self.disputes
            .iter()
            .filter(|d| d.kind == Kind::Meaning)
            .count()
    }

    /// Return the number of visibility-related disputes.
    pub fn visibility_count(&self) -> usize {
        self.disputes
            .iter()
            .filter(|d| d.kind == Kind::Visibility)
            .count()
    }

    /// Return the number of disputes requiring human review.
    pub fn review_count(&self) -> usize {
        self.disputes
            .iter()
            .filter(|d| {
                d.kind == Kind::Meaning && matches!(d.severity, Severity::Review | Severity::High)
            })
            .count()
    }

    /// Check if any dispute in the docket requires human review.
    pub fn requires_review(&self) -> bool {
        self.review_count() > 0
    }

    /// Render the docket as human-readable terminal output.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("  OOT DOCKET\n");
        out.push_str("  ─────────────────────────────────────────\n");
        out.push_str(&format!("  change:     {}\n", self.change));
        out.push_str(&format!("  from:       {}\n", self.source));
        out.push_str(&format!("  base:       {}\n", self.base));
        out.push_str(&format!("  head:       {}\n", self.head));
        out.push('\n');
        out.push_str(&format!(
            "  meaning:    {} disputes detected\n",
            self.meaning_count()
        ));
        if self.visibility_count() > 0 {
            out.push_str(&format!(
                "  visibility: {} private path(s)\n",
                self.visibility_count()
            ));
        }
        out.push('\n');
        out.push_str(&format!("  intent:     {}\n", self.intent));
        out.push_str(&format!("  authors:    {}\n", self.authors.join(", ")));
        out.push('\n');
        if self.disputes.is_empty() {
            out.push_str("  dispute:    none\n");
        } else {
            for (i, d) in self.disputes.iter().enumerate() {
                let tag = match d.kind {
                    Kind::Meaning => "meaning",
                    Kind::Visibility => "visibility",
                };
                out.push_str(&format!(
                    "  dispute-{:02}: {} ({})    [{}]\n",
                    i + 1,
                    d.detail,
                    d.location,
                    tag
                ));
            }
        }
        out.push('\n');
        let label = match self.verdict {
            Verdict::Adjudicated => "ADJUDICATED",
            Verdict::Blocked => "BLOCKED",
            Verdict::Embargoed => "EMBARGOED",
            Verdict::Cloaked => "CLOAKED",
        };
        out.push_str(&format!("  verdict:    ▶ {} ", label));
        let mut notes = Vec::new();
        let reviews = self.review_count();
        if reviews > 0 {
            notes.push(format!("{} requires review", reviews));
        }
        if self.verdict == Verdict::Cloaked {
            notes.push("cloaked".to_string());
        }
        if self.verdict == Verdict::Embargoed {
            notes.push("held for maintainers".to_string());
        }
        if notes.is_empty() {
            out.push('\n');
        } else {
            out.push_str(". ");
            out.push_str(&notes.join(", "));
            out.push('\n');
        }
        if let Some(e) = &self.embargo {
            out.push_str(&format!("  embargo:    {}\n", e));
        }
        out.push('\n');
        out.push_str("  [a]ccept · [r]eject · [d]ocket\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_as_str() {
        assert_eq!(Severity::Low.as_str(), "low");
        assert_eq!(Severity::Review.as_str(), "review");
        assert_eq!(Severity::High.as_str(), "high");
    }

    #[test]
    fn test_docket_render_and_counts() {
        let docket = Docket {
            change: "feature/auth".into(),
            source: "git".into(),
            base: "main".into(),
            head: "feature/auth".into(),
            disputes: vec![
                Dispute {
                    id: "D001".into(),
                    location: "src/lib.rs:1".into(),
                    kind: Kind::Meaning,
                    severity: Severity::Review,
                    detail: "changed login function".into(),
                },
                Dispute {
                    id: "V001".into(),
                    location: ".env".into(),
                    kind: Kind::Visibility,
                    severity: Severity::High,
                    detail: "private path .env touched".into(),
                },
            ],
            intent: "auth refactor".into(),
            authors: vec!["@alice".into(), "@bob".into()],
            verdict: Verdict::Adjudicated,
            embargo: Some("patch held for maintainers until 2026-12-31".into()),
        };

        assert_eq!(docket.meaning_count(), 1);
        assert_eq!(docket.visibility_count(), 1);
        assert_eq!(docket.review_count(), 1);
        assert!(docket.requires_review());

        let rendered = docket.render();
        assert!(rendered.contains("OOT DOCKET"));
        assert!(rendered.contains("feature/auth"));
        assert!(rendered.contains("1 requires review"));
        assert!(rendered.contains("2026-12-31"));
    }
}
