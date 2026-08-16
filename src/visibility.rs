use crate::change::Change;
use crate::dispute::{Dispute, Kind, Severity};
use serde::Deserialize;
use std::path::Path;

/// Declares who may see what, and when a patch may go public.
///
/// This is policy, not cryptography. Actual encryption is delegated to
/// git-crypt or a hosted key service. Oot owns the rule and the gate.
#[derive(Debug, Deserialize)]
pub struct VisibilityPolicy {
    /// Path fragments that are private. A touched path matching any entry
    /// raises a visibility dispute.
    pub private_paths: Vec<String>,
    /// If set, the change is held under embargo until this date.
    pub embargo_until: Option<String>,
    /// Branch names that must stay private.
    pub private_branches: Vec<String>,
}

impl Default for VisibilityPolicy {
    fn default() -> Self {
        VisibilityPolicy {
            private_paths: vec!["secrets/".into(), ".env".into()],
            embargo_until: None,
            private_branches: vec![],
        }
    }
}

impl VisibilityPolicy {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let p: VisibilityPolicy = toml::from_str(&text)?;
        Ok(p)
    }

    /// Emit a visibility dispute for every private path present in the head
    /// snapshot. A private path that simply exists is treated as touched.
    pub fn check(&self, change: &Change) -> Vec<Dispute> {
        let mut out = Vec::new();
        let mut n = 1;
        for path in change.head.files.keys() {
            let private = self
                .private_paths
                .iter()
                .any(|p| path.contains(p.trim_start_matches('/')));
            if private {
                out.push(Dispute {
                    id: format!("V{:03}", n),
                    location: path.clone(),
                    kind: Kind::Visibility,
                    severity: Severity::High,
                    detail: format!(
                        "private path {} touched by {}",
                        path,
                        change.authors.join("/")
                    ),
                });
                n += 1;
            }
        }
        out
    }

    pub fn embargo_note(&self) -> Option<String> {
        self.embargo_until
            .as_ref()
            .map(|date| format!("patch held for maintainers until {}", date))
    }
}
