//! Native Git adapter for extracting in-memory snapshots and adjudicating 3-way merges.

use crate::change::{Change, Snapshot, Source};
use crate::dispute::{Docket, Severity, Verdict};
use crate::engine::Engine;
use crate::policy::MeaningPolicy;
use crate::visibility::VisibilityPolicy;
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Adapter for interacting directly with a Git repository.
#[derive(Debug, Clone)]
pub struct GitAdapter {
    repo_root: PathBuf,
}

/// Configuration options for 3-way Git merge adjudication.
#[derive(Debug, Default, Clone)]
pub struct GitAdjudicateOptions {
    /// Explicit override for the common merge base commit SHA.
    pub custom_merge_base: Option<String>,
    /// Custom identifier or name for the change.
    pub change_name: Option<String>,
    /// Declared intent or purpose of the change.
    pub intent: Option<String>,
}

impl GitAdapter {
    /// Create a new `GitAdapter` rooted at `repo_path`.
    pub fn new(repo_path: impl AsRef<Path>) -> Result<Self> {
        let path = repo_path.as_ref();
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(path)
            .output()
            .with_context(|| format!("Failed to run git in {}", path.display()))?;

        if !output.status.success() {
            return Err(anyhow!(
                "Not a valid git repository at {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let root_str = String::from_utf8(output.stdout)?.trim().to_string();
        Ok(Self {
            repo_root: PathBuf::from(root_str),
        })
    }

    /// Discover a `GitAdapter` from the current working directory.
    pub fn discover() -> Result<Self> {
        Self::new(".")
    }

    /// Returns the absolute path to the repository root.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Resolve a Git reference, branch name, or tag into a canonical commit SHA.
    pub fn resolve_ref(&self, rev: &str) -> Result<String> {
        let output = Command::new("git")
            .args(["rev-parse", "--verify", rev])
            .current_dir(&self.repo_root)
            .output()
            .with_context(|| format!("Failed to resolve git ref '{rev}'"))?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to resolve git ref '{rev}': {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Compute the common ancestor commit SHA (merge-base) between two revisions.
    pub fn merge_base(&self, ref_a: &str, ref_b: &str) -> Result<String> {
        let output = Command::new("git")
            .args(["merge-base", ref_a, ref_b])
            .current_dir(&self.repo_root)
            .output()
            .with_context(|| {
                format!("Failed to compute merge-base between '{ref_a}' and '{ref_b}'")
            })?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to find merge-base between '{ref_a}' and '{ref_b}': {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Extract commit authors across a revision or range (e.g. `base..head`).
    pub fn authors(&self, rev_range: &str) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["log", "--format=%an", rev_range])
            .current_dir(&self.repo_root)
            .output()
            .with_context(|| format!("Failed to query authors for range '{rev_range}'"))?;

        if !output.status.success() {
            // Return empty list rather than hard failing on single refs with no history
            return Ok(Vec::new());
        }

        let text = String::from_utf8(output.stdout)?;
        let mut authors: Vec<String> = text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        authors.sort();
        authors.dedup();
        Ok(authors)
    }

    /// Extract an in-memory `Snapshot` directly from Git object storage without touching the working tree.
    pub fn extract_snapshot(&self, rev: &str) -> Result<Snapshot> {
        let output = Command::new("git")
            .args(["ls-tree", "-r", "-z", rev])
            .current_dir(&self.repo_root)
            .output()
            .with_context(|| format!("Failed to list tree for '{rev}'"))?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to list git tree for '{rev}': {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let raw = output.stdout;
        let mut files = HashMap::new();

        // `git ls-tree -r -z` emits null-terminated records: "<mode> <type> <sha>\t<path>\0"
        for entry in raw.split(|&b| b == 0) {
            if entry.is_empty() {
                continue;
            }
            let entry_str = String::from_utf8_lossy(entry);
            if let Some((meta, path)) = entry_str.split_once('\t') {
                let parts: Vec<&str> = meta.split_whitespace().collect();
                if parts.len() >= 3 && parts[1] == "blob" {
                    let blob_sha = parts[2];
                    let blob_output = Command::new("git")
                        .args(["cat-file", "-p", blob_sha])
                        .current_dir(&self.repo_root)
                        .output()
                        .with_context(|| format!("Failed to fetch blob {blob_sha} for {path}"))?;

                    if blob_output.status.success() {
                        let content = String::from_utf8_lossy(&blob_output.stdout).into_owned();
                        files.insert(path.to_string(), content);
                    }
                }
            }
        }

        Ok(Snapshot { files })
    }

    /// Adjudicate a 3-way Git merge between `base_ref` and `head_ref`.
    ///
    /// Computes the merge-base $M = \text{merge-base}(base\_ref, head\_ref)$, extracts
    /// $S_M$, $S_{base}$, and $S_{head}$, performs 3-way semantic conflict analysis,
    /// checks visibility policies, and produces a finalized [`Docket`].
    pub fn adjudicate_3way(
        &self,
        base_ref: &str,
        head_ref: &str,
        engine: &mut Engine,
        meaning_policy: &MeaningPolicy,
        visibility_policy: &VisibilityPolicy,
        options: &GitAdjudicateOptions,
    ) -> Result<Docket> {
        let base_sha = self.resolve_ref(base_ref)?;
        let head_sha = self.resolve_ref(head_ref)?;

        let merge_base_sha = match &options.custom_merge_base {
            Some(mb) => self.resolve_ref(mb)?,
            None => self.merge_base(&base_sha, &head_sha)?,
        };

        let mb_snapshot = self.extract_snapshot(&merge_base_sha)?;
        let base_snapshot = self.extract_snapshot(&base_sha)?;
        let head_snapshot = self.extract_snapshot(&head_sha)?;

        let mut authors = self.authors(&format!("{merge_base_sha}..{head_sha}"))?;
        if authors.is_empty() {
            authors = vec!["@git-author".to_string()];
        }

        let change_label = options
            .change_name
            .clone()
            .unwrap_or_else(|| format!("{base_ref}..{head_ref}"));

        let change = Change {
            name: change_label,
            source: Source::Git,
            base_ref: format!("{base_ref}@{base_sha:.7}"),
            head_ref: format!("{head_ref}@{head_sha:.7}"),
            base: mb_snapshot.clone(),
            head: head_snapshot.clone(),
            authors: authors.clone(),
            intent: options.intent.clone(),
        };

        let mut disputes = engine.diff_3way(&mb_snapshot, &base_snapshot, &head_snapshot)?;
        let vis_disputes = visibility_policy.check(&change);
        let cloaked = vis_disputes
            .iter()
            .any(|d| d.kind == crate::dispute::Kind::Visibility && d.severity == Severity::High);
        disputes.extend(vis_disputes);

        let verdict = if cloaked {
            Verdict::Cloaked
        } else if visibility_policy.embargo_until.is_some() {
            Verdict::Embargoed
        } else {
            meaning_policy.evaluate(&disputes)
        };

        let mut touched_paths: Vec<String> = base_snapshot
            .files
            .keys()
            .chain(head_snapshot.files.keys())
            .filter(|p| base_snapshot.files.get(*p) != head_snapshot.files.get(*p))
            .cloned()
            .collect();
        touched_paths.sort();
        touched_paths.dedup();

        let scope = if touched_paths.is_empty() {
            "no files changed".to_string()
        } else {
            touched_paths.join(", ")
        };

        let docket = Docket {
            change: change.name,
            source: format!("git: {merge_base_sha:.7} (base) vs {head_sha:.7} (head)"),
            base: change.base_ref,
            head: change.head_ref,
            disputes,
            scope,
            authors,
            verdict,
            embargo: visibility_policy.embargo_note(),
        };

        Ok(docket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_adapter_discover_in_repo() {
        let adapter = GitAdapter::discover();
        assert!(adapter.is_ok(), "Expected discovery in git repository");
        let adapter = adapter.unwrap();
        assert!(adapter.repo_root().exists());
    }

    #[test]
    fn test_git_adapter_resolve_head() {
        let adapter = GitAdapter::discover().expect("git repo");
        let head_sha = adapter.resolve_ref("HEAD");
        assert!(head_sha.is_ok());
        let sha = head_sha.unwrap();
        assert_eq!(sha.len(), 40, "SHA should be 40 characters");
    }

    #[test]
    fn test_git_adapter_extract_snapshot_head() {
        let adapter = GitAdapter::discover().expect("git repo");
        let snapshot = adapter.extract_snapshot("HEAD");
        assert!(snapshot.is_ok());
        let snap = snapshot.unwrap();
        assert!(snap.files.contains_key("Cargo.toml"));
        assert!(snap.files.contains_key("src/lib.rs") || snap.files.contains_key("src/main.rs"));
    }
}
