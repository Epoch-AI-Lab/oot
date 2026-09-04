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
#[derive(Debug, Clone, Deserialize)]
pub struct VisibilityPolicy {
    /// Path fragments that are private. A touched path matching any entry
    /// raises a visibility dispute.
    #[serde(default = "default_private_paths")]
    pub private_paths: Vec<String>,
    /// If set, the change is held under embargo until this date (e.g. `YYYY-MM-DD`).
    #[serde(default)]
    pub embargo_until: Option<String>,
    /// Branch names that must stay private. Referencing these raises a visibility dispute.
    #[serde(default)]
    pub private_branches: Vec<String>,
}

fn default_private_paths() -> Vec<String> {
    vec!["secrets/".into(), ".env".into()]
}

impl Default for VisibilityPolicy {
    fn default() -> Self {
        VisibilityPolicy {
            private_paths: default_private_paths(),
            embargo_until: None,
            private_branches: vec![],
        }
    }
}

fn matches_pattern(candidate: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        candidate.starts_with(prefix) && candidate[prefix.len()..].ends_with(suffix)
    } else {
        candidate == pattern
    }
}

impl VisibilityPolicy {
    /// Load a visibility policy from a TOML configuration file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let p: VisibilityPolicy = toml::from_str(&text)?;
        Ok(p)
    }

    /// Whether a branch name matches any declared private branch pattern.
    pub fn branch_is_private(&self, branch: &str) -> bool {
        self.private_branches.iter().any(|pb| {
            let pb = pb.trim_start_matches('/');
            matches_pattern(branch, pb)
                || branch == pb
                || branch.starts_with(&format!("{pb}/"))
                || branch.ends_with(&format!("/{pb}"))
                || branch.contains(&format!("/{pb}/"))
        })
    }

    /// Whether a touched path matches any private-path fragment.
    /// The single matching semantic shared by adjudication and export filtering.
    pub fn path_is_private(&self, path: &str) -> bool {
        let clean_path = path.trim_start_matches('/');
        let filename = clean_path
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(clean_path);
        self.private_paths.iter().any(|pattern| {
            let pat = pattern.trim_start_matches('/');
            if pat.is_empty() {
                return false;
            }
            if pat.contains('*') {
                return matches_pattern(clean_path, pat) || matches_pattern(filename, pat);
            }
            if pat.ends_with('/') {
                let dir_pat = pat.trim_end_matches('/');
                clean_path == dir_pat
                    || clean_path.starts_with(pat)
                    || clean_path.contains(&format!("/{pat}"))
            } else {
                clean_path == pat
                    || clean_path.starts_with(&format!("{pat}/"))
                    || clean_path.ends_with(&format!("/{pat}"))
                    || clean_path.contains(&format!("/{pat}/"))
                    || filename == pat
                    || (pat.starts_with('.')
                        && (filename.starts_with(&format!("{pat}."))
                            || filename.starts_with(&format!("{pat}_"))
                            || filename == pat))
            }
        })
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

        // Check private paths among files the change touches (added, modified, or deleted)
        let mut touched_paths: Vec<&String> = Vec::new();
        for (path, head_content) in &change.head.files {
            let touched = change
                .base
                .files
                .get(path)
                .is_none_or(|base_content| base_content != head_content);
            if touched {
                touched_paths.push(path);
            }
        }
        for path in change.base.files.keys() {
            if !change.head.files.contains_key(path) {
                touched_paths.push(path);
            }
        }
        touched_paths.sort();
        touched_paths.dedup();

        for path in touched_paths {
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
            if !branch_clean.is_empty() {
                let pb = branch_clean.trim_start_matches('/');
                let matched = matches_pattern(&change.name, pb)
                    || matches_pattern(&change.head_ref, pb)
                    || matches_pattern(&change.base_ref, pb)
                    || change.name.contains(pb)
                    || change.head_ref.contains(pb)
                    || change.base_ref.contains(pb);
                if matched {
                    out.push(Dispute {
                        id: format!("V{:03}", n),
                        location: format!("branch:{}", branch_clean),
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
        }

        out
    }

    /// Generate an embargo notification note if an embargo date is active.
    pub fn embargo_note(&self) -> Option<String> {
        self.embargo_until
            .as_ref()
            .map(|date| format!("patch held for maintainers until {}", date))
    }

    /// Whether the repository or change is currently under an active embargo.
    pub fn is_under_embargo(&self) -> bool {
        if let Some(date) = &self.embargo_until {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let days = now / 86400;
            let (cur_y, cur_m, cur_d) = days_to_ymd(days);
            let today = format!("{:04}-{:02}-{:02}", cur_y, cur_m, cur_d);
            date.trim() >= today.as_str()
        } else {
            false
        }
    }
}

/// Parse calendar date string in common standard formats (YYYY-MM-DD, YYYY/MM/DD, YYYY.MM.DD, DD-MM-YYYY, DD/MM/YYYY, DD.MM.YYYY).
pub fn parse_date_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(['-', '/', '.']).collect();
    if parts.len() != 3 {
        return None;
    }
    let p0: i64 = parts[0].parse().ok()?;
    let p1: u32 = parts[1].parse().ok()?;
    let p2: i64 = parts[2].parse().ok()?;

    let (y, m, d) = if p0 >= 1000 {
        (p0, p1, p2 as u32)
    } else if p2 >= 1000 {
        (p2, p1, p0 as u32)
    } else {
        return None;
    };

    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y <= 0 {
        return None;
    }
    Some((y, m, d))
}

fn days_to_ymd(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
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
