//! Visibility policy evaluation and embargo management.
//!
//! Visibility is the governance spine of Oot: declaring who may see what,
//! which paths/branches are restricted, and when patches may be released publicly.

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
    /// If set, the change is held under embargo until this date (e.g. `YYYY-MM-DD`).
    pub embargo_until: Option<String>,
    /// Branch names that must stay private. Referencing these raises a visibility dispute.
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
    /// Load a visibility policy from a TOML configuration file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let p: VisibilityPolicy = toml::from_str(&text)?;
        Ok(p)
    }

    /// Whether a touched path matches any private-path fragment.
    /// The single matching semantic shared by adjudication and export filtering.
    pub fn path_is_private(&self, path: &str) -> bool {
        self.private_paths
            .iter()
            .any(|p| path.contains(p.trim_start_matches('/')))
    }

    /// Evaluate visibility rules against a change.
    ///
    /// Emits a visibility dispute for:
    /// - Every private path *touched* by the change: present in the head
    ///   snapshot but absent from base, or with different content. Files that
    ///   already existed unchanged are not touched, even if they match a
    ///   private-path fragment.
    /// - Every private branch referenced by name or refs.
    pub fn check(&self, change: &Change) -> Vec<Dispute> {
        let mut out = Vec::new();
        let mut n = 1;

        // Check private paths among files the change actually touches
        for (path, head_content) in &change.head.files {
            let touched = change
                .base
                .files
                .get(path)
                .is_none_or(|base_content| base_content != head_content);
            if !touched {
                continue;
            }
            if self.path_is_private(path) {
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

        // Check private branches
        for branch in &self.private_branches {
            let branch_clean = branch.trim();
            if !branch_clean.is_empty()
                && (change.name.contains(branch_clean)
                    || change.head_ref.contains(branch_clean)
                    || change.base_ref.contains(branch_clean))
            {
                out.push(Dispute {
                    id: format!("V{:03}", n),
                    location: change.name.clone(),
                    kind: Kind::Visibility,
                    severity: Severity::High,
                    detail: format!(
                        "private branch {} referenced by {}",
                        branch_clean,
                        change.authors.join("/")
                    ),
                });
                n += 1;
            }
        }

        out
    }

    /// Generate an embargo notification note if an embargo date is active.
    pub fn embargo_note(&self) -> Option<String> {
        self.embargo_until
            .as_ref()
            .map(|date| format!("patch held for maintainers until {}", date))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{Snapshot, Source};

    #[test]
    fn test_visibility_policy_private_paths_and_branches() {
        let policy = VisibilityPolicy {
            private_paths: vec!["secrets/".into(), ".env".into()],
            embargo_until: Some("2026-10-01".into()),
            private_branches: vec!["internal-audit".into()],
        };

        let mut head = Snapshot::default();
        head.files.insert("secrets/key.pem".into(), "secret".into());

        let change = Change {
            name: "feature/internal-audit".into(),
            source: Source::Git,
            base_ref: "main".into(),
            head_ref: "feature/internal-audit".into(),
            base: Snapshot::default(),
            head,
            authors: vec!["@agent".into()],
            intent: None,
        };

        let disputes = policy.check(&change);
        assert_eq!(disputes.len(), 2);
        assert!(disputes.iter().all(|d| d.kind == Kind::Visibility));
        assert_eq!(
            policy.embargo_note().as_deref(),
            Some("patch held for maintainers until 2026-10-01")
        );
    }

    #[test]
    fn test_visibility_policy_clean_and_default() {
        let default_policy = VisibilityPolicy::default();
        assert_eq!(default_policy.private_paths, vec!["secrets/", ".env"]);
        assert_eq!(default_policy.embargo_until, None);
        assert!(default_policy.private_branches.is_empty());
        assert_eq!(default_policy.embargo_note(), None);

        let mut head = Snapshot::default();
        head.files
            .insert("src/main.rs".into(), "fn main() {}".into());

        let change = Change {
            name: "feature/public".into(),
            source: Source::Git,
            base_ref: "main".into(),
            head_ref: "feature/public".into(),
            base: Snapshot::default(),
            head,
            authors: vec!["@alice".into()],
            intent: None,
        };

        let disputes = default_policy.check(&change);
        assert!(disputes.is_empty());
    }

    #[test]
    fn test_visibility_policy_only_flags_touched_private_paths() {
        let policy = VisibilityPolicy::default();

        let mut base = Snapshot::default();
        // Private file already exists, unchanged by this change.
        base.files.insert("secrets/key.pem".into(), "same".into());
        base.files.insert("src/lib.rs".into(), "fn a() {}".into());

        let mut head = Snapshot::default();
        head.files.insert("secrets/key.pem".into(), "same".into());
        head.files.insert("src/lib.rs".into(), "fn b() {}".into());

        let change = Change {
            name: "feature/public".into(),
            source: Source::Git,
            base_ref: "main".into(),
            head_ref: "feature/public".into(),
            base,
            head,
            authors: vec!["@alice".into()],
            intent: None,
        };

        let disputes = policy.check(&change);
        assert!(
            disputes.is_empty(),
            "unchanged private files must not be flagged, got {:?}",
            disputes
        );

        // Now modify the private file: it becomes touched and must flag.
        let mut head2 = change.head.clone();
        head2
            .files
            .insert("secrets/key.pem".into(), "rotated".into());

        let change2 = Change {
            head: head2,
            ..change
        };

        let disputes = policy.check(&change2);
        assert_eq!(disputes.len(), 1);
        assert_eq!(disputes[0].location, "secrets/key.pem");
    }
}
