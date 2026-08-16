//! VCS adapters for extracting snapshots directly from version control systems.

pub mod git;

pub use git::{GitAdapter, GitAdjudicateOptions};
