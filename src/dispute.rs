use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Meaning,
    Visibility,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Review,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dispute {
    pub id: String,
    pub location: String,
    pub kind: Kind,
    pub severity: Severity,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Adjudicated,
    Blocked,
    Embargoed,
    Cloaked,
}

/// The full adjudication record for one Change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Docket {
    pub change: String,
    pub source: String,
    pub base: String,
    pub head: String,
    pub disputes: Vec<Dispute>,
    pub scope: String,
    pub authors: Vec<String>,
    pub verdict: Verdict,
    pub embargo: Option<String>,
}

impl Docket {
    pub fn meaning_count(&self) -> usize {
        self.disputes.iter().filter(|d| d.kind == Kind::Meaning).count()
    }

    pub fn visibility_count(&self) -> usize {
        self.disputes.iter().filter(|d| d.kind == Kind::Visibility).count()
    }

    pub fn requires_review(&self) -> bool {
        self.disputes.iter().any(|d| {
            d.kind == Kind::Meaning && matches!(d.severity, Severity::Review | Severity::High)
        })
    }

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
        out.push_str(&format!("  scope:      {}\n", self.scope));
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
        if self.requires_review() {
            notes.push("1 requires review".to_string());
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
