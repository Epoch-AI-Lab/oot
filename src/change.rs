use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Git,
    Jj,
    Memory,
}

impl Source {
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

/// A snapshot is a set of file paths to their contents. Oot never assumes these
/// live on disk; they can come from git, Jujutsu, or an agent's memory.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub files: HashMap<String, String>,
}

/// A Change is the only thing Oot adjudicates: a content-addressed delta
/// between two snapshots, with declared intent, visibility, and authorship.
#[derive(Debug, Clone)]
pub struct Change {
    pub name: String,
    pub source: Source,
    pub base_ref: String,
    pub head_ref: String,
    pub base: Snapshot,
    pub head: Snapshot,
    pub authors: Vec<String>,
    pub intent: Option<String>,
}
