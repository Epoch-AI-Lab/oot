//! The court side of the store: adjudicating a stored change straight from
//! its trees, and persisting the verdict as sidecar governance data under
//! `.oot/`.
//!
//! Sidecar layout (governance never mutates [`ChangeRecord`] or content
//! addressing):
//! - `.oot/dockets/<change-id>.json` — the latest docket for a change,
//!   overwritten on every re-run.
//! - `.oot/adjudications.jsonl` — append-only audit trail, one line per run,
//!   mirroring `export-log.jsonl` style.
//!
//! Coupling stays loose: dockets reference change ids; neither the DAG nor
//! export ever references dockets.

use crate::change::{Change, Snapshot, Source};
use crate::dispute::{finalize_adjudication, Docket, Kind, Severity};
use crate::engine::Engine;
use crate::policy::MeaningPolicy;
use crate::store::{now_epoch, ChangeRecord, Store};
use crate::visibility::VisibilityPolicy;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Directory under `.oot/` holding persisted dockets.
pub const DOCKETS_DIR: &str = "dockets";
/// Append-only audit log under `.oot/`, one adjudication per line.
pub const ADJUDICATIONS_LOG: &str = "adjudications.jsonl";
/// Envelope schema version; bump on any shape change.
pub const DOCKET_SCHEMA: u32 = 1;

/// The persisted envelope at `.oot/dockets/<change-id>.json`: provenance
/// wrapped around the rendered [`Docket`] verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedDocket {
    pub schema: u32,
    /// Full change id this docket belongs to.
    pub change: String,
    /// Head tree sha the adjudication ran against.
    pub tree: String,
    /// Parent change ids; the first parent is the v1 base.
    pub parents: Vec<String>,
    /// Seconds since the Unix epoch when this run happened.
    pub adjudicated_at: u64,
    /// Fingerprint of the meaning + visibility policies used.
    pub policy_key: String,
    /// The rendered docket, exactly as printed.
    pub docket: Docket,
}

/// Adjudicate stored change `id`: head is the change's own tree, base is its
/// FIRST parent's tree (a root change diffs against an empty snapshot), and
/// authors come from the record. Merge changes are judged against their
/// first parent in v1. Provenance follows `source_sha`: `[git]` for imported
/// changes, `[oot]` for native ones.
///
/// `intent` and `authors` override the record-derived defaults when given.
pub fn adjudicate_change(
    store: &Store,
    id: &str,
    engine: &Engine,
    meaning_policy: &MeaningPolicy,
    visibility_policy: &VisibilityPolicy,
    intent: Option<String>,
    authors_override: Option<Vec<String>>,
) -> Result<PersistedDocket> {
    let record = store.get_change(id)?;
    let head = store.snapshot_from_tree(&record.tree)?;
    let base = match record.parents.first() {
        Some(parent) => store.snapshot_from_tree(&store.get_change(parent)?.tree)?,
        None => Snapshot::default(),
    };

    // Provenance tag matches `oot log`: [git] for imported, [oot] for native.
    let imported = record.source_sha.is_some();
    let authors = authors_override.unwrap_or_else(|| vec![record.author.name.clone()]);
    let short = short_id(id);

    let change = Change {
        name: short.clone(),
        source: if imported {
            Source::Git
        } else {
            Source::Memory
        },
        base_ref: base_label(&record),
        head_ref: short.clone(),
        base,
        head,
        authors,
        intent: intent.clone(),
    };

    let vis_disputes = visibility_policy.check(&change);
    let cloaked = vis_disputes
        .iter()
        .any(|d| d.kind == Kind::Visibility && d.severity == Severity::High);
    let mut disputes = engine.diff_snapshots(&change.base, &change.head)?;
    disputes.extend(vis_disputes);

    let (disputes, intent, verdict) = finalize_adjudication(
        disputes,
        &change.base.files,
        &change.head.files,
        intent.clone(),
        cloaked,
        visibility_policy.embargo_until.is_some(),
        meaning_policy,
    );

    let docket = Docket {
        change: short,
        source: if imported { "git" } else { "oot" }.to_string(),
        base: change.base_ref.clone(),
        head: change.head_ref.clone(),
        disputes,
        intent,
        authors: change.authors.clone(),
        verdict,
        embargo: visibility_policy.embargo_note(),
    };

    Ok(PersistedDocket {
        schema: DOCKET_SCHEMA,
        change: id.to_string(),
        tree: record.tree.clone(),
        parents: record.parents.clone(),
        adjudicated_at: now_epoch(),
        policy_key: policy_key(meaning_policy, visibility_policy),
        docket,
    })
}

/// Persist the envelope to `.oot/dockets/<id>.json`, overwriting any previous
/// adjudication of the same change.
pub fn save_docket(store: &Store, persisted: &PersistedDocket) -> Result<()> {
    let dir = store.path().join(DOCKETS_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", persisted.change));
    std::fs::write(&path, serde_json::to_vec_pretty(persisted)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Load the persisted envelope for an already-resolved change id.
pub fn load_docket(store: &Store, id: &str) -> Result<PersistedDocket> {
    let path = store.path().join(DOCKETS_DIR).join(format!("{id}.json"));
    let bytes = std::fs::read(&path).with_context(|| {
        format!(
            "no persisted docket for {} (run `oot adjudicate --change {}`)",
            short_id(id),
            short_id(id)
        )
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Append one audit line to `.oot/adjudications.jsonl`: append-only history,
/// not cache — re-running a change adds a line rather than replacing one.
pub fn log_adjudication(store: &Store, persisted: &PersistedDocket) -> Result<()> {
    let entry = serde_json::json!({
        "epoch": now_epoch(),
        "event": "adjudicated",
        "change": persisted.change,
        "verdict": persisted.docket.verdict,
        "meaning": persisted.docket.meaning_count(),
        "visibility": persisted.docket.visibility_count(),
        "policy_key": persisted.policy_key,
    });
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store.path().join(ADJUDICATIONS_LOG))?;
    writeln!(f, "{entry}")?;
    Ok(())
}

/// Stable fingerprint of the two policies an adjudication ran under — the
/// export cache key's trick applied to governance: canonical serialization,
/// then hash. Any new policy field MUST join the canonical string or stale
/// keys will look equal after a policy change.
pub fn policy_key(meaning: &MeaningPolicy, visibility: &VisibilityPolicy) -> String {
    let mut canon = String::new();
    canon.push_str("block_on=");
    push_list(&mut canon, &meaning.block_on);
    canon.push_str(";review_on=");
    push_list(&mut canon, &meaning.review_on);
    canon.push_str(";private_paths=");
    push_list(&mut canon, &visibility.private_paths);
    canon.push_str(";private_branches=");
    push_list(&mut canon, &visibility.private_branches);
    canon.push_str(";embargo_until=");
    canon.push_str(visibility.embargo_until.as_deref().unwrap_or(""));
    fnv1a(canon.as_bytes())
}

fn push_list(out: &mut String, items: &[String]) {
    for item in items {
        out.push_str(item);
        out.push('\u{1f}');
    }
}

/// FNV-1a 64-bit, hex-encoded. Not cryptographic — it only has to be stable
/// and good enough to notice that a policy file changed.
fn fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Display form of a change id, matching what `oot log` prints.
fn short_id(id: &str) -> String {
    id.chars().take(7).collect()
}

/// Docket label for the base snapshot: the first parent's short id, or
/// `(root)` when the change has no parents.
fn base_label(record: &ChangeRecord) -> String {
    match record.parents.first() {
        Some(p) => short_id(p),
        None => "(root)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Identity, WorkFile};

    fn temp_store(tag: &str) -> (std::path::PathBuf, Store) {
        let tmp = std::env::temp_dir().join(format!("oot-court-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let store = Store::init(&tmp).unwrap();
        (tmp, store)
    }

    fn identity() -> Identity {
        Identity {
            name: "Kriday".into(),
            email: "k@oot.dev".into(),
            time: 1_700_000_000,
            offset: "+0530".into(),
        }
    }

    #[test]
    fn test_read_blob_and_snapshot_from_tree_roundtrip() {
        let (_tmp, store) = temp_store("snap");
        let files = vec![
            WorkFile {
                path: "lib.rs".into(),
                contents: b"pub fn greet() -> &'static str { \"hi\" }\n".to_vec(),
                executable: false,
            },
            WorkFile {
                path: "deep/nested/bin.dat".into(),
                contents: vec![0x00, 0xff, 0x7f, 0x80],
                executable: false,
            },
            WorkFile {
                path: "run.sh".into(),
                contents: b"#!/bin/sh\n".to_vec(),
                executable: true,
            },
        ];
        let tree = store.write_tree_from_files(&files).unwrap();

        let snap = store.snapshot_from_tree(&tree).unwrap();
        assert_eq!(snap.files.len(), 3);
        assert_eq!(
            snap.files["lib.rs"],
            b"pub fn greet() -> &'static str { \"hi\" }\n".to_vec()
        );
        assert_eq!(
            snap.files["deep/nested/bin.dat"],
            vec![0x00, 0xff, 0x7f, 0x80]
        );

        let entries = store.tree_files(&tree).unwrap();
        let (sha, _) = entries.get("run.sh").unwrap();
        assert_eq!(store.read_blob(sha).unwrap(), b"#!/bin/sh\n".to_vec());
        assert!(store.read_blob("does-not-exist").is_err());
    }

    #[test]
    fn test_docket_save_load_roundtrip_and_overwrite() {
        let (_tmp, store) = temp_store("docket");
        let mk = |at: u64| PersistedDocket {
            schema: DOCKET_SCHEMA,
            change: "abc1234567890".into(),
            tree: "tree-sha".into(),
            parents: vec![],
            adjudicated_at: at,
            policy_key: "key-1".into(),
            docket: Docket {
                change: "abc1234".into(),
                source: "oot".into(),
                base: "(root)".into(),
                head: "abc1234".into(),
                disputes: vec![],
                intent: "no files changed".into(),
                authors: vec!["Kriday".into()],
                verdict: crate::dispute::Verdict::Adjudicated,
                embargo: None,
            },
        };

        save_docket(&store, &mk(42)).unwrap();
        let loaded = load_docket(&store, "abc1234567890").unwrap();
        assert_eq!(loaded.adjudicated_at, 42);
        assert_eq!(loaded.docket.change, "abc1234");
        assert_eq!(loaded.policy_key, "key-1");
        assert_eq!(loaded.schema, DOCKET_SCHEMA);

        // Re-running overwrites the sidecar instead of stacking copies.
        save_docket(&store, &mk(43)).unwrap();
        assert_eq!(
            load_docket(&store, "abc1234567890").unwrap().adjudicated_at,
            43
        );

        // Unknown id fails loudly with the next-step hint.
        let err = load_docket(&store, "ffffffff").unwrap_err().to_string();
        assert!(err.contains("no persisted docket"), "{err}");
    }

    #[test]
    fn test_audit_log_appends_one_line_per_run() {
        let (_tmp, store) = temp_store("audit");
        let persisted = PersistedDocket {
            schema: DOCKET_SCHEMA,
            change: "aaaa1111".into(),
            tree: "t".into(),
            parents: vec![],
            adjudicated_at: 1,
            policy_key: "k".into(),
            docket: Docket {
                change: "aaaa111".into(),
                source: "oot".into(),
                base: "(root)".into(),
                head: "aaaa111".into(),
                disputes: vec![crate::dispute::Dispute::empty_change()],
                intent: "no files changed".into(),
                authors: vec!["K".into()],
                verdict: crate::dispute::Verdict::Adjudicated,
                embargo: None,
            },
        };

        log_adjudication(&store, &persisted).unwrap();
        log_adjudication(&store, &persisted).unwrap();

        let log = std::fs::read_to_string(store.path().join(ADJUDICATIONS_LOG)).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(lines[0].contains("\"event\":\"adjudicated\""), "{log}");
        assert!(lines[0].contains("\"change\":\"aaaa1111\""), "{log}");
        assert!(lines[0].contains("\"verdict\":\"adjudicated\""), "{log}");
        assert!(lines[0].contains("\"meaning\":0"), "{log}");
        assert!(lines[0].contains("\"policy_key\":\"k\""), "{log}");
    }

    #[test]
    fn test_policy_key_stable_and_sensitive() {
        let base = policy_key(&MeaningPolicy::default(), &VisibilityPolicy::default());
        assert_eq!(base.len(), 16);
        // Deterministic across calls.
        assert_eq!(
            base,
            policy_key(&MeaningPolicy::default(), &VisibilityPolicy::default())
        );

        let strict = MeaningPolicy {
            block_on: vec!["review".into()],
            ..MeaningPolicy::default()
        };
        assert_ne!(base, policy_key(&strict, &VisibilityPolicy::default()));

        let embargo = VisibilityPolicy {
            embargo_until: Some("2026-09-01".into()),
            ..Default::default()
        };
        assert_ne!(base, policy_key(&MeaningPolicy::default(), &embargo));

        let extra_path = VisibilityPolicy {
            private_paths: vec!["secrets/".into(), ".env".into(), "vault/".into()],
            ..Default::default()
        };
        assert_ne!(base, policy_key(&MeaningPolicy::default(), &extra_path));
    }

    #[test]
    fn test_adjudicate_root_change_against_empty_base() {
        let (_tmp, store) = temp_store("root");
        let files = vec![WorkFile {
            path: "lib.rs".into(),
            contents: b"pub fn greet() -> &'static str { \"hi\" }\n".to_vec(),
            executable: false,
        }];
        let record = ChangeRecord {
            parents: vec![],
            tree: store.write_tree_from_files(&files).unwrap(),
            author: identity(),
            committer: identity(),
            message: "root\n".into(),
            source_sha: None,
        };
        let id = store.put_record(&record).unwrap();

        let engine = Engine::new().unwrap();
        let p = adjudicate_change(
            &store,
            &id,
            &engine,
            &MeaningPolicy::default(),
            &VisibilityPolicy::default(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(p.schema, DOCKET_SCHEMA);
        assert_eq!(p.change, id);
        assert_eq!(p.parents, Vec::<String>::new());
        assert_eq!(p.docket.source, "oot", "native changes carry the oot tag");
        assert_eq!(p.docket.base, "(root)");
        assert_eq!(p.docket.authors, vec!["Kriday".to_string()]);
        // Whole-tree additions are Review-level, which the default policy
        // does not block on.
        assert_eq!(p.docket.verdict, crate::dispute::Verdict::Adjudicated);
        assert!(p
            .docket
            .disputes
            .iter()
            .any(|d| d.detail.contains("file added")));
    }

    #[test]
    fn test_adjudicate_child_change_against_first_parent() {
        let (_tmp, store) = temp_store("child");
        let base_tree = store
            .write_tree_from_files(&[WorkFile {
                path: "lib.rs".into(),
                contents: b"pub fn calc() -> i32 { 1 }\n".to_vec(),
                executable: false,
            }])
            .unwrap();
        let base_record = ChangeRecord {
            parents: vec![],
            tree: base_tree,
            author: identity(),
            committer: identity(),
            message: "base\n".into(),
            source_sha: Some("0123abcd".into()),
        };
        let base_id = store.put_record(&base_record).unwrap();

        let child_tree = store
            .write_tree_from_files(&[WorkFile {
                path: "lib.rs".into(),
                contents: b"pub fn calc() -> i32 { 2 }\n".to_vec(),
                executable: false,
            }])
            .unwrap();
        let child_record = ChangeRecord {
            parents: vec![base_id.clone()],
            tree: child_tree,
            author: identity(),
            committer: identity(),
            message: "edit calc\n".into(),
            source_sha: Some("0123abce".into()),
        };
        let child_id = store.put_record(&child_record).unwrap();

        let engine = Engine::new().unwrap();
        let p = adjudicate_change(
            &store,
            &child_id,
            &engine,
            &MeaningPolicy::default(),
            &VisibilityPolicy::default(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(p.parents, vec![base_id.clone()]);
        assert_eq!(p.docket.source, "git", "imported changes carry the git tag");
        assert_eq!(p.docket.base, short_id(&base_id));
        assert_eq!(p.docket.verdict, crate::dispute::Verdict::Adjudicated);
        assert!(p
            .docket
            .disputes
            .iter()
            .any(|d| d.detail.contains("both sides changed `calc`")));
    }
}
