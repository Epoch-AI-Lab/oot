//! Native Jujutsu (jj) adapter for extracting in-memory snapshots and adjudicating 3-way merges.
//!
//! Shells out to the `jj` binary rather than linking `jj-lib`, because the library
//! crate is internal to the jj CLI and its API is unstable. All calls are read-only
//! and pass `--ignore-working-copy` so adjudication never snapshots or mutates the
//! caller's working copy.

use crate::change::{Change, Snapshot, Source};
use crate::dispute::{Dispute, Docket, Kind, Severity, Verdict};
use crate::engine::Engine;
use crate::policy::MeaningPolicy;
use crate::visibility::VisibilityPolicy;
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Adapter for interacting directly with a Jujutsu repository.
#[derive(Debug, Clone)]
pub struct JjAdapter {
    repo_root: PathBuf,
}

/// Configuration options for 3-way Jujutsu merge adjudication.
#[derive(Debug, Default, Clone)]
pub struct JjAdjudicateOptions {
    /// Explicit override for the common ancestor commit ID (revset).
    pub custom_ancestor: Option<String>,
    /// Custom identifier or name for the change.
    pub change_name: Option<String>,
    /// Declared intent or purpose of the change.
    pub intent: Option<String>,
}

/// Shorten a commit ID for display (commit IDs are ASCII hex, so slicing is safe).
fn short(id: &str) -> &str {
    &id[..id.len().min(7)]
}

impl JjAdapter {
    /// Create a new `JjAdapter` rooted at the Jujutsu workspace containing `repo_path`.
    pub fn new(repo_path: impl AsRef<Path>) -> Result<Self> {
        let path = repo_path.as_ref();
        let output = Command::new("jj")
            .args(["--ignore-working-copy", "--no-pager", "root"])
            .current_dir(path)
            .output()
            .with_context(|| {
                format!(
                    "Failed to run jj in {} (is Jujutsu installed?)",
                    path.display()
                )
            })?;

        if !output.status.success() {
            return Err(anyhow!(
                "Not a valid jj repository at {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let root_str = String::from_utf8(output.stdout)?.trim().to_string();
        Ok(Self {
            repo_root: PathBuf::from(root_str),
        })
    }

    /// Discover a `JjAdapter` from the current working directory.
    pub fn discover() -> Result<Self> {
        Self::new(".")
    }

    /// Returns the absolute path to the repository root.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Run a read-only jj command and return its stdout.
    fn run(&self, args: &[&str]) -> Result<String> {
        let mut full: Vec<&str> = vec!["--ignore-working-copy", "--no-pager", "--quiet"];
        full.extend_from_slice(args);

        let output = Command::new("jj")
            .args(&full)
            .current_dir(&self.repo_root)
            .output()
            .with_context(|| format!("Failed to run jj {:?}", args))?;

        if !output.status.success() {
            return Err(anyhow!(
                "jj {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Resolve a revset to exactly one commit ID.
    ///
    /// Revset symbols resolve by priority (tag, then bookmark, then commit/change ID),
    /// so callers passing ambiguous input should wrap it (e.g. `bookmarks(exact:name)`
    /// or `commit_id(prefix)`) before calling this.
    pub fn resolve_commit_id(&self, revset: &str) -> Result<String> {
        let out = self.run(&[
            "log",
            "--no-graph",
            "-T",
            "commit_id ++ \"\\n\"",
            "-r",
            revset,
        ])?;

        let ids: Vec<&str> = out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        match ids.len() {
            0 => Err(anyhow!("jj revset '{revset}' matched no commits")),
            1 => Ok(ids[0].to_string()),
            n => Err(anyhow!(
                "jj revset '{revset}' matched {n} commits; expected exactly one"
            )),
        }
    }

    /// Resolve a revset to exactly one change ID (stable across history rewrites).
    pub fn resolve_change_id(&self, revset: &str) -> Result<String> {
        let out = self.run(&[
            "log",
            "--no-graph",
            "-T",
            "change_id ++ \"\\n\"",
            "-r",
            revset,
        ])?;

        let ids: Vec<&str> = out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        match ids.len() {
            0 => Err(anyhow!("jj revset '{revset}' matched no commits")),
            1 => Ok(ids[0].to_string()),
            n => Err(anyhow!(
                "jj revset '{revset}' matched {n} commits; expected exactly one"
            )),
        }
    }

    /// Compute the common ancestor commit ID between two commits.
    ///
    /// Uses the revset `heads(::a & ::b)` (the fork point). Criss-cross histories can
    /// yield multiple fork points; this errors in that case and the caller should
    /// supply [`JjAdjudicateOptions::custom_ancestor`] explicitly.
    pub fn ancestor(&self, commit_a: &str, commit_b: &str) -> Result<String> {
        let revset = format!("heads(::{commit_a} & ::{commit_b})");
        self.resolve_commit_id(&revset).with_context(|| {
            format!(
                "Failed to compute common ancestor between '{commit_a}' and '{commit_b}'; \
                 pass an explicit ancestor with --merge-base"
            )
        })
    }

    /// Extract commit authors across the range `ancestor..head`.
    pub fn authors(&self, ancestor: &str, head: &str) -> Result<Vec<String>> {
        let revset = format!("{ancestor}..{head}");
        let out = match self.run(&[
            "log",
            "--no-graph",
            "-T",
            "author.name() ++ \"\\n\"",
            "-r",
            &revset,
        ]) {
            Ok(o) => o,
            Err(_) => return Ok(Vec::new()),
        };

        let mut authors: Vec<String> = out
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        authors.sort();
        authors.dedup();
        Ok(authors)
    }

    /// Extract an in-memory `Snapshot` from a revision without touching the working copy.
    ///
    /// Files in a conflicted state are excluded from the snapshot (their materialized
    /// text is conflict markers, not real content); they are returned separately so the
    /// caller can raise a dispute instead of feeding markers to the parser.
    pub fn extract_snapshot_with_conflicts(&self, rev: &str) -> Result<(Snapshot, Vec<String>)> {
        let listing = self.run(&["file", "list", "-r", rev])?;

        let mut files = HashMap::new();
        let mut conflicted = Vec::new();

        for path in listing.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let content = self.run(&["file", "show", "-r", rev, "--", path])?;
            if content
                .lines()
                .any(|l| l.starts_with("<<<<<<<") && l.contains("conflict"))
            {
                conflicted.push(path.to_string());
            } else {
                files.insert(path.to_string(), content);
            }
        }

        Ok((Snapshot { files }, conflicted))
    }

    /// Extract an in-memory `Snapshot` from a revision, dropping conflicted files.
    pub fn extract_snapshot(&self, rev: &str) -> Result<Snapshot> {
        Ok(self.extract_snapshot_with_conflicts(rev)?.0)
    }

    /// Adjudicate a 3-way merge between `base_ref` and `head_ref`.
    ///
    /// Resolves both revsets to commit IDs, computes the common ancestor (or uses
    /// `options.custom_ancestor`), extracts the three snapshots in memory, runs
    /// structural analysis plus visibility policy, and produces a finalized [`Docket`].
    pub fn adjudicate_3way(
        &self,
        base_ref: &str,
        head_ref: &str,
        engine: &Engine,
        meaning_policy: &MeaningPolicy,
        visibility_policy: &VisibilityPolicy,
        options: &JjAdjudicateOptions,
    ) -> Result<Docket> {
        let base_id = self.resolve_commit_id(base_ref)?;
        let head_id = self.resolve_commit_id(head_ref)?;

        let ancestor_id = match &options.custom_ancestor {
            Some(a) => self.resolve_commit_id(a)?,
            None => self.ancestor(&base_id, &head_id)?,
        };

        let (ancestor_snapshot, _) = self.extract_snapshot_with_conflicts(&ancestor_id)?;
        let (base_snapshot, _) = self.extract_snapshot_with_conflicts(&base_id)?;
        let (head_snapshot, head_conflicts) = self.extract_snapshot_with_conflicts(&head_id)?;

        let mut authors = self.authors(&ancestor_id, &head_id)?;
        if authors.is_empty() {
            authors = vec!["@jj-author".to_string()];
        }

        let change_label = options
            .change_name
            .clone()
            .unwrap_or_else(|| format!("{base_ref}..{head_ref}"));

        let change = Change {
            name: change_label,
            source: Source::Jj,
            base_ref: format!("{base_ref}@{}", short(&base_id)),
            head_ref: format!("{head_ref}@{}", short(&head_id)),
            base: ancestor_snapshot.clone(),
            head: head_snapshot.clone(),
            authors: authors.clone(),
            intent: options.intent.clone(),
        };

        let mut disputes = engine.diff_3way(&ancestor_snapshot, &base_snapshot, &head_snapshot)?;

        // Conflicted files carry logical conflicts jj materializes as markers;
        // raise them directly rather than parsing marker text as source.
        for path in &head_conflicts {
            let id = format!("D{:03}", disputes.len() + 1);
            disputes.push(Dispute {
                id,
                location: path.clone(),
                kind: Kind::Meaning,
                severity: Severity::High,
                detail: format!(
                    "conflicted file `{}` carries unresolved merge conflict; resolve before merge",
                    path
                ),
            });
        }

        let vis_disputes = visibility_policy.check(&change);
        let cloaked = vis_disputes
            .iter()
            .any(|d| d.kind == Kind::Visibility && d.severity == Severity::High);
        disputes.extend(vis_disputes);

        let mut touched_paths: Vec<String> = base_snapshot
            .files
            .keys()
            .chain(head_snapshot.files.keys())
            .filter(|p| base_snapshot.files.get(*p) != head_snapshot.files.get(*p))
            .cloned()
            .collect();
        touched_paths.sort();
        touched_paths.dedup();

        if touched_paths.is_empty() {
            disputes.push(Dispute::empty_change());
        }

        let verdict = if cloaked {
            Verdict::Cloaked
        } else if visibility_policy.embargo_until.is_some() {
            Verdict::Embargoed
        } else {
            meaning_policy.evaluate(&disputes)
        };

        let intent = options.intent.clone().unwrap_or_else(|| {
            if touched_paths.is_empty() {
                "no files changed".to_string()
            } else {
                touched_paths.join(", ")
            }
        });

        let docket = Docket {
            change: change.name,
            source: format!(
                "jj: {} (base) vs {} (head)",
                short(&ancestor_id),
                short(&head_id)
            ),
            base: change.base_ref,
            head: change.head_ref,
            disputes,
            intent,
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
    fn test_short_truncates_commit_id() {
        assert_eq!(short("0123456789abcdef"), "0123456");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
    }
}
