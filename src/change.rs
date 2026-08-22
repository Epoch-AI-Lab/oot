//! Change models and snapshot representations.
//!
//! A [`Change`] is the fundamental unit of adjudication in Oot:
//! a content-addressed delta between two snapshots with declared intent,
//! visibility policy, and authorship.

use std::collections::HashMap;

/// The origin system or VCS from which a change was ingested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Ingested from a Git repository or reference.
    Git,
    /// Ingested from a Jujutsu (jj) workspace or bookmark.
    Jj,
    /// Ingested directly from an in-memory buffer or agent runtime.
    Memory,
}

impl Source {
    /// Return the canonical string identifier for this source.
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Git => "git",
            Source::Jj => "jj",
            Source::Memory => "memory",
        }
    }
}

impl std::str::FromStr for Source {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "git" => Ok(Source::Git),
            "jj" | "jujutsu" => Ok(Source::Jj),
            "memory" | "mem" => Ok(Source::Memory),
            other => anyhow::bail!("unknown source: {}", other),
        }
    }
}

/// A snapshot is a mapping of relative file paths to their contents.
///
/// Contents are stored as raw bytes so binary files compare exactly;
/// text conversion happens only when the structural engine parses a file.
/// Oot never assumes these files exist on a physical filesystem;
/// they can be ingested from git, Jujutsu, or an agent's memory isolate.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Map of file path (relative to repo root) to raw file content.
    pub files: HashMap<String, Vec<u8>>,
}

/// A Change is the core unit Oot adjudicates: a content-addressed delta
/// between two snapshots, with declared intent, visibility, and authorship.
#[derive(Debug, Clone)]
pub struct Change {
    /// Human-readable identifier or branch/change name.
    pub name: String,
    /// VCS or execution environment origin.
    pub source: Source,
    /// Identifier or commit/tree hash for the base snapshot.
    pub base_ref: String,
    /// Identifier or commit/tree hash for the head snapshot.
    pub head_ref: String,
    /// Base snapshot representing the state before the change.
    pub base: Snapshot,
    /// Head snapshot representing the state after the change.
    pub head: Snapshot,
    /// List of author handles or agent identities (e.g. `@kriday`, `@agent-7`).
    pub authors: Vec<String>,
    /// Declared intent, purpose, or summary of the change.
    pub intent: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_parsing_and_str() {
        assert_eq!("git".parse::<Source>().unwrap(), Source::Git);
        assert_eq!("jj".parse::<Source>().unwrap(), Source::Jj);
        assert_eq!("jujutsu".parse::<Source>().unwrap(), Source::Jj);
        assert_eq!("memory".parse::<Source>().unwrap(), Source::Memory);
        assert_eq!("mem".parse::<Source>().unwrap(), Source::Memory);
        assert!("invalid".parse::<Source>().is_err());

        assert_eq!(Source::Git.as_str(), "git");
        assert_eq!(Source::Jj.as_str(), "jj");
        assert_eq!(Source::Memory.as_str(), "memory");
    }

    #[test]
    fn test_change_and_snapshot_creation() {
        let mut snap = Snapshot::default();
        snap.files
            .insert("src/lib.rs".into(), "pub fn test() {}".as_bytes().to_vec());

        let change = Change {
            name: "test-change".into(),
            source: Source::Git,
            base_ref: "main".into(),
            head_ref: "feature".into(),
            base: Snapshot::default(),
            head: snap,
            authors: vec!["@alice".into()],
            intent: Some("implement test feature".into()),
        };

        assert_eq!(change.name, "test-change");
        assert_eq!(change.intent.as_deref(), Some("implement test feature"));
        assert_eq!(change.authors.len(), 1);
        assert_eq!(change.head.files.len(), 1);
    }
}
