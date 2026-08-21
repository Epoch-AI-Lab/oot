//! VCS adapters for extracting snapshots directly from version control systems.

pub mod git;
pub mod jj;

pub use git::{GitAdapter, GitAdjudicateOptions};
pub use jj::{JjAdapter, JjAdjudicateOptions};
