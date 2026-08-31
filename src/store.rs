//! The Oot store: Oot's own record of history, kept in `.oot/`.
//!
//! Layout:
//! - `.oot/objects.git/` — a bare Git object database. Storage is delegated to
//!   Git's odb (content addressing, dedup, corruption resistance) while the
//!   model stays Oot's: changes, not commits.
//! - `.oot/changes/<id>.json` — one [`ChangeRecord`] per change.
//! - `.oot/map/<commit-sha>` — original commit SHA to change id, so imports
//!   are idempotent and exports can verify round-tripping.
//! - `.oot/refs/<name>` — head change id for each imported branch.
//!
//! A change id is the `git hash-object` SHA of its canonical JSON, so records
//! are content-addressed like everything else in the store.

use crate::change::Snapshot;
use crate::visibility::VisibilityPolicy;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory name created inside a project root by `oot init`.
pub const STORE_DIR: &str = ".oot";

const OBJECTS_DIR: &str = "objects.git";
const CHANGES_DIR: &str = "changes";
const MAP_DIR: &str = "map";
const REFS_DIR: &str = "refs";
const EXPORT_LOG: &str = "export-log.jsonl";
/// Git's well-known empty tree; used to diff root commits against nothing.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Author or committer identity plus the exact timestamp needed to
/// reproduce a byte-identical Git commit on export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub email: String,
    /// Unix timestamp (seconds).
    pub time: i64,
    /// Raw Git timezone offset, e.g. `+0530`.
    pub offset: String,
}

impl Identity {
    /// Value for `GIT_AUTHOR_DATE` / `GIT_COMMITTER_DATE` that preserves the
    /// timestamp and timezone exactly (`<unix-ts> <offset>` is git's raw form).
    pub fn date_env(&self) -> String {
        format!("{} {}", self.time, self.offset)
    }
}

/// One change in the store: the full metadata of an original commit,
/// decoupled from any particular VCS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub parents: Vec<String>,
    /// Tree object sha for this change's snapshot (present in the store's odb).
    pub tree: String,
    pub author: Identity,
    pub committer: Identity,
    pub message: String,
    /// The original VCS commit this change was imported from, if any. Lets
    /// export reuse the original commit object verbatim — preserving extra
    /// headers like `gpgsig` and `encoding` that [`Identity`] cannot express —
    /// whenever every ancestor exports to its own original sha too.
    #[serde(default)]
    pub source_sha: Option<String>,
}

/// One file captured from a working copy, ready to become a tree entry.
#[derive(Debug, Clone)]
pub struct WorkFile {
    /// Path relative to the project root, `/`-separated.
    pub path: String,
    pub contents: Vec<u8>,
    pub executable: bool,
}

/// An open handle on a project's `.oot/` directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Create a fresh store at `<root>/.oot`. Fails if one already exists.
    pub fn init(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let oot = root.join(STORE_DIR);
        if oot.exists() {
            bail!("store already exists at {}", oot.display());
        }
        std::fs::create_dir_all(oot.join(CHANGES_DIR))?;
        std::fs::create_dir_all(oot.join(MAP_DIR))?;
        std::fs::create_dir_all(oot.join(REFS_DIR))?;
        std::fs::create_dir_all(oot.join("export"))?;
        run(Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(oot.join(OBJECTS_DIR))
            .current_dir(root))?;
        Ok(Self { root: oot })
    }

    /// Open an existing store discovered at or above `start`.
    pub fn open(start: impl AsRef<Path>) -> Result<Self> {
        let start = start
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| start.as_ref().to_path_buf());
        let mut dir: Option<&Path> = Some(start.as_path());
        while let Some(d) = dir {
            let candidate = d.join(STORE_DIR);
            if candidate.is_dir() && candidate.join(OBJECTS_DIR).is_dir() {
                return Ok(Self { root: candidate });
            }
            dir = d.parent();
        }
        bail!(
            "no Oot store found at or above {} (run `oot init`)",
            start.display()
        );
    }

    /// Path of the `.oot` directory itself.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Path usable as `--git-dir` for commands that read or write the odb.
    pub fn git_dir(&self) -> PathBuf {
        self.root.join(OBJECTS_DIR)
    }

    /// Fetch all branches from a source repository into the store's odb.
    /// Objects become local, so deleting the source repo loses nothing.
    pub fn fetch_branches(&self, source_repo: &Path) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
            .current_dir(source_repo)
            .output()
            .context("failed to list branches in source repository")?;
        if !output.status.success() {
            bail!("not a valid git repository: {}", source_repo.display());
        }
        let branches: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();

        run(Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["fetch", "--quiet", "--no-tags"])
            .arg(source_repo)
            .args(["+refs/heads/*:refs/oot/source/*"]))?;

        Ok(branches)
    }

    /// Walk a ref's history in the source repository, oldest first, emitting
    /// one [`RawCommit`] per commit. Fields are NUL-separated and records are
    /// terminated by `\x01`; `git log` also inserts a bare newline between
    /// entries, which is stripped from the start of every record after the
    /// first. A message containing these control bytes fails loudly on
    /// validation rather than silently misaligning.
    pub fn log_raw(&self, source_repo: &Path, branch: &str) -> Result<Vec<RawCommit>> {
        let fmt =
            "%H%x00%T%x00%P%x00%an%x00%ae%x00%at%x00%aI%x00%cn%x00%ce%x00%ct%x00%cI%x00%B%x01";
        let output = Command::new("git")
            .args(["log", "--topo-order", "--reverse"])
            .arg(format!("--pretty=format:{fmt}"))
            .arg(branch)
            .current_dir(source_repo)
            .output()
            .context("failed to read history from source repository")?;
        if !output.status.success() {
            bail!(
                "failed to read history for branch '{branch}': {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let mut commits = Vec::new();
        for (i, record) in output.stdout.split(|&b| b == 0x01).enumerate() {
            let mut record = record;
            if i > 0 && record.first() == Some(&b'\n') {
                record = &record[1..];
            }
            if record.is_empty() {
                continue;
            }
            commits.push(RawCommit::parse(record)?);
        }
        Ok(commits)
    }

    /// Store a parsed commit as a [`ChangeRecord`], returning its change id.
    /// Idempotent: re-importing the same original commit returns the existing id.
    pub fn put_commit(&self, raw: &RawCommit) -> Result<String> {
        let map_file = self.root.join(MAP_DIR).join(&raw.sha);
        if map_file.exists() {
            return Ok(std::fs::read_to_string(&map_file)?.trim().to_string());
        }

        // Parents are stored as change ids, not original commit SHAs, so the
        // store forms a self-contained DAG in Oot's own address space. Import
        // order guarantees parents are already stored.
        let mut parent_ids = Vec::with_capacity(raw.parents.len());
        for p in &raw.parents {
            let pid = self
                .change_for_commit(p)?
                .ok_or_else(|| anyhow!("parent commit {p} of {} not yet imported", raw.sha))?;
            parent_ids.push(pid);
        }

        let record = ChangeRecord {
            parents: parent_ids,
            tree: raw.tree.clone(),
            author: Identity {
                name: raw.author_name.clone(),
                email: raw.author_email.clone(),
                time: raw.author_time,
                offset: parse_offset(&raw.author_iso)?,
            },
            committer: Identity {
                name: raw.committer_name.clone(),
                email: raw.committer_email.clone(),
                time: raw.committer_time,
                offset: parse_offset(&raw.committer_iso)?,
            },
            message: raw.message.clone(),
            source_sha: Some(raw.sha.clone()),
        };

        self.put_record(&record)
    }

    /// Persist a [`ChangeRecord`] under its content address. Idempotent:
    /// identical records return the existing id. Records carrying a
    /// `source_sha` are registered in the original-commit map so re-imports
    /// dedupe and exports can verify round-tripping.
    pub fn put_record(&self, record: &ChangeRecord) -> Result<String> {
        let json = serde_json::to_vec(record)?;
        let id = self.hash_object(&json, "blob", false)?;

        std::fs::write(
            self.root.join(CHANGES_DIR).join(format!("{id}.json")),
            &json,
        )?;
        if let Some(orig) = &record.source_sha {
            std::fs::write(self.root.join(MAP_DIR).join(orig), &id)?;
        }
        Ok(id)
    }

    /// `git hash-object` against the store's odb. Record ids are hashed
    /// without writing (records live as JSON files, not odb objects); blobs
    /// are written so trees can reference them.
    fn hash_object(&self, bytes: &[u8], kind: &str, write: bool) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.args(["hash-object", "-t", kind]);
        if write {
            cmd.arg("-w");
        }
        cmd.arg("--stdin")
            .env("GIT_DIR", self.git_dir())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("failed to run git hash-object")?;
        use std::io::Write;
        child
            .stdin
            .take()
            .context("hash-object has no stdin")?
            .write_all(bytes)?;
        let hash = child.wait_with_output()?;
        if !hash.status.success() {
            bail!(
                "hash-object failed: {}",
                String::from_utf8_lossy(&hash.stderr).trim()
            );
        }
        Ok(String::from_utf8(hash.stdout)?.trim().to_string())
    }

    /// The head change id of `branch`, if the branch has any.
    /// Branch names are percent-encoded (`%` -> `%25`, `/` -> `%2F`)
    /// so the mapping is unambiguous and reversible.
    pub fn head_id(&self, branch: &str) -> Result<Option<String>> {
        let safe = encode_branch(branch);
        let f = self.root.join(REFS_DIR).join(safe);
        if !f.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(f)?.trim().to_string()))
    }

    /// Every blob under `tree`: (path, blob sha, executable). Reads straight
    /// from the store's odb; no checkout involved.
    pub fn tree_files(&self, tree: &str) -> Result<HashMap<String, (String, bool)>> {
        let out = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["ls-tree", "-r", "-z", tree])
            .output()
            .context("failed to read the tree from the store's odb")?;
        if !out.status.success() {
            bail!(
                "git ls-tree failed for {tree}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let mut map = HashMap::new();
        for entry in out.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
            let record = String::from_utf8_lossy(entry);
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| anyhow!("malformed ls-tree entry: {record}"))?;
            let mut parts = meta.split(' ');
            let mode = parts.next().unwrap_or_default();
            let _kind = parts.next().unwrap_or_default();
            let sha = parts.next().unwrap_or_default();
            validate_tree_path(path)?;
            map.insert(path.to_string(), (sha.to_string(), mode == "100755"));
        }
        Ok(map)
    }

    /// Content address of `bytes` as a blob, without storing it. Lets callers
    /// compare working-copy content against trees without polluting the odb.
    pub fn blob_sha(&self, bytes: &[u8]) -> Result<String> {
        self.hash_object(bytes, "blob", false)
    }

    /// Read one blob's exact bytes from the store's odb.
    pub fn read_blob(&self, sha: &str) -> Result<Vec<u8>> {
        let out = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["cat-file", "blob", sha])
            .output()
            .context("failed to read a blob from the store's odb")?;
        if !out.status.success() {
            bail!(
                "cat-file blob {sha} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    /// Rebuild a [`Snapshot`] from a tree in the store's odb: `ls-tree -r -z`
    /// lists the blobs, `read_blob` fetches each one. Gitlinks (submodules)
    /// are skipped — they point at other commits rather than hold content,
    /// so there is nothing to adjudicate.
    pub fn snapshot_from_tree(&self, tree: &str) -> Result<Snapshot> {
        let listed = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["ls-tree", "-r", "-z", tree])
            .output()
            .context("failed to read the tree from the store's odb")?;
        if !listed.status.success() {
            bail!(
                "git ls-tree failed for {tree}: {}",
                String::from_utf8_lossy(&listed.stderr).trim()
            );
        }

        let mut snap = Snapshot::default();
        for entry in listed.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
            let record = String::from_utf8_lossy(entry);
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| anyhow!("malformed ls-tree entry: {record}"))?;
            let mut parts = meta.splitn(3, ' ');
            let _mode = parts.next().unwrap_or_default();
            let kind = parts.next().unwrap_or_default();
            let sha = parts.next().unwrap_or_default();
            if kind != "blob" {
                continue;
            }
            // FATAL fix: sanitize paths coming verbatim from git ls-tree.
            validate_tree_path(path)?;
            snap.files.insert(path.to_string(), self.read_blob(sha)?);
        }
        Ok(snap)
    }

    /// Resolve a change id or unique prefix to its full id. Exact match wins;
    /// otherwise the prefix must select exactly one stored change or this
    /// fails loudly listing every candidate.
    pub fn resolve_change(&self, id_or_prefix: &str) -> Result<String> {
        if id_or_prefix.contains('/') || id_or_prefix.contains('\\') || id_or_prefix.contains("..")
        {
            bail!("invalid change id '{id_or_prefix}'");
        }
        let changes = self.root.join(CHANGES_DIR);
        if changes.join(format!("{id_or_prefix}.json")).exists() {
            return Ok(id_or_prefix.to_string());
        }
        let mut candidates: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&changes)? {
            let name = entry?.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".json") {
                if stem.starts_with(id_or_prefix) {
                    candidates.push(stem.to_string());
                }
            }
        }
        candidates.sort();
        match candidates.as_slice() {
            [] => bail!("no change matching '{id_or_prefix}' in store (see `oot log`)"),
            [one] => Ok(one.clone()),
            many => bail!(
                "ambiguous change prefix '{id_or_prefix}' matches {} changes:\n{}",
                many.len(),
                many.iter()
                    .map(|c| format!("  {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        }
    }

    /// Store file contents as blobs in the odb and assemble them into a
    /// nested tree, returning the root tree sha. Paths use `/` separators;
    /// empty directories cannot be represented and are skipped naturally.
    pub fn write_tree_from_files(&self, files: &[WorkFile]) -> Result<String> {
        let hashed: Vec<(String, String, bool)> = files
            .iter()
            .map(|f| Ok((f.path.clone(), self.write_blob(&f.contents)?, f.executable)))
            .collect::<Result<_>>()?;
        self.assemble_tree("", &hashed)
    }

    /// Recursively build the tree for all paths under directory `dir`
    /// ("" = root). Grouping by first path component keeps each level local.
    fn assemble_tree(&self, dir: &str, files: &[(String, String, bool)]) -> Result<String> {
        // (sort key, mktree row)
        let mut rows: Vec<(String, String)> = Vec::new();
        let mut subdirs: HashMap<String, Vec<(String, String, bool)>> = HashMap::new();

        for (path, sha, exec) in files {
            match path.split_once('/') {
                None => {
                    let mode = if *exec { "100755" } else { "100644" };
                    rows.push((path.clone(), format!("{mode} blob {sha}\t{path}")));
                }
                Some((head, rest)) => {
                    subdirs.entry(head.to_string()).or_default().push((
                        rest.to_string(),
                        sha.clone(),
                        *exec,
                    ));
                }
            }
        }

        for (name, children) in subdirs {
            let child_prefix = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let sha = self.assemble_tree(&child_prefix, &children)?;
            // Git sorts tree entries as if directory names ended with '/'.
            rows.push((format!("{name}/"), format!("040000 tree {sha}\t{name}")));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));

        let input = rows
            .into_iter()
            .map(|(_, row)| row)
            .collect::<Vec<_>>()
            .join("\n");
        let mut child = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .arg("mktree")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to run git mktree")?;
        use std::io::Write;
        child
            .stdin
            .take()
            .context("mktree has no stdin")?
            .write_all(input.as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "git mktree failed for dir '{dir}': {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    fn write_blob(&self, bytes: &[u8]) -> Result<String> {
        self.hash_object(bytes, "blob", true)
    }

    /// Load a change record by id.
    pub fn get_change(&self, id: &str) -> Result<ChangeRecord> {
        if id.contains('/') || id.contains('\\') || id.contains("..") {
            bail!("invalid change id '{id}'");
        }
        let path = self.root.join(CHANGES_DIR).join(format!("{id}.json"));
        let bytes =
            std::fs::read(&path).with_context(|| format!("change {id} not found in store"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// All change ids in import order (the append-only index).
    pub fn index(&self) -> Result<Vec<String>> {
        let path = self.root.join(".index");
        if !path.exists() {
            return Ok(Vec::new());
        }
        Ok(std::fs::read_to_string(path)?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Append a change id to the import-order index (skips duplicates).
    pub fn index_push(&self, id: &str) -> Result<()> {
        use std::io::Write;
        if self.index()?.iter().any(|e| e == id) {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(".index"))?;
        writeln!(f, "{id}")?;
        Ok(())
    }

    /// Record the head change id for a branch.
    pub fn set_ref(&self, branch: &str, id: &str) -> Result<()> {
        let safe = encode_branch(branch);
        std::fs::write(self.root.join(REFS_DIR).join(safe), id)?;
        Ok(())
    }

    /// Read all recorded branches as (branch, head change id).
    pub fn refs(&self) -> Result<Vec<(String, String)>> {
        let dir = self.root.join(REFS_DIR);
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.path().is_file() {
                continue;
            }
            let raw = entry.file_name().to_string_lossy().to_string();
            let name = decode_branch(&raw);
            let id = std::fs::read_to_string(entry.path())?.trim().to_string();
            out.push((name, id));
        }
        out.sort();
        Ok(out)
    }

    /// Resolve an original commit sha to its stored change id, if imported.
    pub fn change_for_commit(&self, sha: &str) -> Result<Option<String>> {
        let f = self.root.join(MAP_DIR).join(sha);
        if !f.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(f)?.trim().to_string()))
    }

    /// Replay every indexed change into `out_repo` (which must be an
    /// initialized git repository) as real commits. The store's odb is
    /// attached via `GIT_ALTERNATE_OBJECT_DIRECTORIES`, so trees and blobs are
    /// read without copying; `git push` transfers them natively later.
    ///
    /// Each change takes one of two paths:
    /// - Identity fast path: when the original commit object lives in the
    ///   store's odb and every parent exported to its own original sha, the
    ///   change reuses the original object verbatim. This keeps bytes that a
    ///   reconstruction cannot express — GPG signatures, encodings, mergetags —
    ///   so signed merges round-trip byte-identically.
    /// - Reconstruction path: otherwise `git commit-tree` rebuilds the commit
    ///   from preserved author/committer/timestamps/message/tree plus remapped
    ///   parents (used downstream of filtered or rewritten history).
    ///
    /// With a visibility policy whose `private_paths` are non-empty, export
    /// runs filtered: every change touching a private path is withheld, every
    /// kept tree is rewritten minus those paths, children remap to their
    /// nearest kept ancestors, and changes left empty by stripping are
    /// skipped. Every withholding decision lands in `.oot/export-log.jsonl`.
    /// Untouched commits and clean prefixes take the identity fast path on a
    /// per-commit basis, preserving original commit hashes and GPG signatures.
    pub fn replay(
        &self,
        out_repo: &Path,
        policy: Option<&VisibilityPolicy>,
    ) -> Result<Vec<(String, String)>> {
        // Attach the store's odb permanently so every later git operation in
        // the exported repo (update-ref, log, push) resolves our objects.
        // Content addressing means commits already present via alternates are
        // not rewritten; they are simply visible.
        let alt_dir = out_repo.join(".git/objects/info");
        std::fs::create_dir_all(&alt_dir)?;
        std::fs::write(
            alt_dir.join("alternates"),
            self.git_dir()
                .join("objects")
                .canonicalize()?
                .as_os_str()
                .as_encoded_bytes(),
        )?;

        let filtering =
            policy.is_some_and(|p| !p.private_paths.is_empty() || !p.private_branches.is_empty());

        // Export mappings are only valid for the policy they were produced
        // under: a filtered export's shas mean nothing to an unfiltered one
        // and vice versa. A changed policy wipes the cache before anything
        // can silently mix decisions from two regimes.
        let filter_key = match policy {
            Some(p) if filtering => {
                format!(
                    "{}\u{1f}{}\u{1f}{}",
                    p.private_paths.join(","),
                    p.private_branches.join(","),
                    p.embargo_until.as_deref().unwrap_or_default()
                )
            }
            _ => String::new(),
        };
        self.reset_export_cache_if_policy_changed(&filter_key)?;

        // Taint pass: decide once, up front, which changes touch private paths.
        let mut withheld: HashMap<String, String> = HashMap::new();
        if filtering {
            let pol = policy.expect("checked above");
            for id in self.index()? {
                let record = self.get_change(&id)?;
                let hits: Vec<String> = self
                    .touched_paths(&record)?
                    .into_iter()
                    .filter(|p| pol.path_is_private(p))
                    .collect();
                if !hits.is_empty() {
                    withheld.insert(id, format!("private path match: {}", hits.join(", ")));
                }
            }
        }

        let mut sha_of: HashMap<String, String> = HashMap::new();
        let mut source_sha_of: HashMap<String, String> = HashMap::new();
        // Exported sha -> rebuilt tree sha (filtered mode only).
        let mut tree_of: HashMap<String, String> = HashMap::new();
        let mut exported = Vec::new();

        for id in self.index()? {
            let record = self.get_change(&id)?;
            if let Some(sha) = self.exported_sha(&id)? {
                if filtering {
                    let tree = self.strip_tree(&record.tree, policy.unwrap(), "")?;
                    tree_of.insert(sha.clone(), tree);
                }
                sha_of.insert(id.clone(), sha.clone());
                if record.source_sha.as_deref() == Some(&sha) {
                    source_sha_of.insert(id.clone(), sha.clone());
                }
                exported.push((id, sha));
                continue;
            }

            // In filtered mode, check if the change was tainted and withheld.
            if filtering {
                if let Some(reason) = withheld.get(&id) {
                    self.log_withheld(&id, record.source_sha.as_deref(), reason)?;
                    continue;
                }
            }

            // Stripped tree for filtered exports, original tree otherwise.
            let stripped_tree = if filtering {
                self.strip_tree(&record.tree, policy.unwrap(), "")?
            } else {
                record.tree.clone()
            };

            // Filtered path: check if the change was left empty over its kept ancestry.
            let parent_shas: Vec<String> = if filtering {
                let mut ps = Vec::new();
                for p in &record.parents {
                    for anc in self.nearest_kept(p, &sha_of)? {
                        if !ps.contains(&anc) {
                            ps.push(anc);
                        }
                    }
                }
                let empty_over_ancestry =
                    !ps.is_empty() && ps.iter().all(|p| tree_of.get(p) == Some(&stripped_tree));
                if empty_over_ancestry {
                    let why = "empty after private-path stripping".to_string();
                    self.log_withheld(&id, record.source_sha.as_deref(), &why)?;
                    continue;
                }
                ps
            } else {
                let missing = record
                    .parents
                    .iter()
                    .filter(|p| !sha_of.contains_key(*p))
                    .count();
                if missing > 0 {
                    bail!("change {id} references unexported parents");
                }
                record.parents.iter().map(|p| sha_of[p].clone()).collect()
            };

            // Identity fast path: reuse the original commit object when the
            // tree was untouched by filtering and the whole ancestry below is
            // byte-exact. This preserves GPG signatures, encodings, and
            // commit SHAs across clean history prefixes and untouched subtrees.
            if let Some(orig) = &record.source_sha {
                let tree_untouched = stripped_tree == record.tree;
                let parents_exact = record.parents.iter().all(|p| {
                    sha_of
                        .get(p)
                        .is_some_and(|e| source_sha_of.get(p) == Some(e))
                });
                if tree_untouched && parents_exact && self.commit_object_exists(orig)? {
                    std::fs::write(self.export_map_path(&id), orig)?;
                    sha_of.insert(id.clone(), orig.clone());
                    source_sha_of.insert(id.clone(), orig.clone());
                    if filtering {
                        tree_of.insert(orig.clone(), stripped_tree);
                    }
                    exported.push((id.clone(), orig.clone()));
                    continue;
                }
            }

            // Reconstruction path: rebuild commit with remapped parents.
            let mut cmd = self.commit_tree_cmd(&stripped_tree, &record);
            for p in &parent_shas {
                cmd.args(["-p", p]);
            }
            let sha = self.finish_commit(cmd, &record.message, &id)?;
            if filtering {
                tree_of.insert(sha.clone(), stripped_tree);
            }
            sha_of.insert(id.clone(), sha.clone());
            exported.push((id.clone(), sha));
        }
        Ok(exported)
    }

    /// Wipe cached export mappings when the visibility policy changed since
    /// the last export. The audit log survives: it is append-only history,
    /// not cache.
    fn reset_export_cache_if_policy_changed(&self, filter_key: &str) -> Result<()> {
        let export_dir = self.root.join("export");
        std::fs::create_dir_all(&export_dir)?;
        let marker = export_dir.join("policy-key");
        let prev = std::fs::read_to_string(&marker).unwrap_or_default();
        if prev == filter_key {
            return Ok(());
        }
        for entry in std::fs::read_dir(&export_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == "policy-key" || name == EXPORT_LOG {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
        std::fs::write(&marker, filter_key)?;
        Ok(())
    }

    /// Nearest exported ancestors of change `id`, walking up through any
    /// changes that were withheld or skipped. FIFO queue preserves parent order.
    fn nearest_kept(&self, id: &str, sha_of: &HashMap<String, String>) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(id.to_string());
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(id.to_string());
        while let Some(cur) = queue.pop_front() {
            if let Some(sha) = sha_of.get(&cur) {
                if !out.contains(sha) {
                    out.push(sha.clone());
                }
                continue;
            }
            for p in self.get_change(&cur)?.parents {
                if seen.insert(p.clone()) {
                    queue.push_back(p);
                }
            }
        }
        Ok(out)
    }

    /// The exported head commit for a branch whose head change is `head_id`,
    /// walking up through withheld/skipped changes. `None` means the branch's
    /// entire history was withheld and the ref should be omitted.
    pub fn branch_head_sha(&self, head_id: &str) -> Result<Option<String>> {
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(head_id.to_string());
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(head_id.to_string());
        while let Some(cur) = queue.pop_front() {
            if let Some(sha) = self.exported_sha(&cur)? {
                return Ok(Some(sha));
            }
            for p in self.get_change(&cur)?.parents {
                if seen.insert(p.clone()) {
                    queue.push_back(p);
                }
            }
        }
        Ok(None)
    }

    /// Paths a change touches relative to each of its parents, read straight
    /// from the store's odb. A merge's touched set is the union over its
    /// parents; root commits diff against git's empty tree.
    pub fn touched_paths(&self, record: &ChangeRecord) -> Result<Vec<String>> {
        let parent_trees: Vec<String> = if record.parents.is_empty() {
            vec![EMPTY_TREE.to_string()]
        } else {
            record
                .parents
                .iter()
                .map(|p| Ok(self.get_change(p)?.tree))
                .collect::<Result<Vec<_>>>()?
        };
        let mut out = Vec::new();
        for pt in parent_trees {
            let output = Command::new("git")
                .args(["--git-dir"])
                .arg(self.git_dir())
                .args(["diff-tree", "-r", "-z", "--name-only", &pt, &record.tree])
                .output()
                .context("failed to diff trees in the store's odb")?;
            if !output.status.success() {
                bail!(
                    "diff-tree failed for {}: {}",
                    record.tree,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            out.extend(
                output
                    .stdout
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).to_string()),
            );
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Rebuild `tree` minus every path matching the policy's private
    /// fragments, recursively. Pure plumbing (`ls-tree` + `mktree`) against
    /// the store's bare odb — no index or worktree involved. Deterministic:
    /// identical inputs yield the original sha untouched.
    fn strip_tree(&self, tree: &str, policy: &VisibilityPolicy, prefix: &str) -> Result<String> {
        let listed = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["ls-tree", "-z", tree])
            .output()
            .context("failed to read a tree while stripping")?;
        if !listed.status.success() {
            bail!(
                "git ls-tree failed for {tree}: {}",
                String::from_utf8_lossy(&listed.stderr).trim()
            );
        }

        let mut lines: Vec<String> = Vec::new();
        let mut changed = false;
        for entry in listed.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
            let record = String::from_utf8_lossy(entry);
            let (meta, name) = record
                .split_once('\t')
                .ok_or_else(|| anyhow!("malformed ls-tree entry: {record}"))?;
            let path = format!("{prefix}{name}");
            let mut parts = meta.splitn(3, ' ');
            let mode = parts.next().unwrap_or_default().to_string();
            let kind = parts.next().unwrap_or_default().to_string();
            let sha = parts.next().unwrap_or_default().to_string();

            match kind.as_str() {
                "commit" => {
                    if policy.path_is_private(&path) {
                        changed = true;
                        continue;
                    }
                    lines.push(record.to_string());
                }
                "blob" => {
                    if policy.path_is_private(&path) {
                        changed = true;
                        continue;
                    }
                    lines.push(record.to_string());
                }
                "tree" => {
                    let sub = self.strip_tree(&sha, policy, &format!("{path}/"))?;
                    if sub != sha {
                        changed = true;
                    }
                    if sub != EMPTY_TREE {
                        lines.push(format!("{mode} tree {sub}\t{name}"));
                    } else {
                        changed = true;
                    }
                }
                other => bail!("unexpected entry kind '{other}' in tree {tree}"),
            }
        }

        if !changed {
            return Ok(tree.to_string());
        }

        if lines.is_empty() {
            return Ok(EMPTY_TREE.to_string());
        }

        use std::io::Write;
        let mut child = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["mktree", "-z"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to run git mktree")?;
        let mut payload = Vec::new();
        for line in lines {
            payload.extend_from_slice(line.as_bytes());
            payload.push(0);
        }
        child
            .stdin
            .take()
            .context("mktree has no stdin")?
            .write_all(&payload)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "git mktree failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// A `git commit-tree` invocation preset with this record's identity and
    /// timestamps; callers append `-p <sha>` per parent and pipe the message.
    /// Writes into the store's odb so cached shas resolve in every export.
    fn commit_tree_cmd(&self, tree: &str, record: &ChangeRecord) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(["--git-dir"])
            .arg(self.git_dir())
            .arg("commit-tree")
            .arg(tree)
            .env(
                "GIT_AUTHOR_NAME",
                record.author.name.replace('\n', " ").replace('\r', ""),
            )
            .env(
                "GIT_AUTHOR_EMAIL",
                record.author.email.replace('\n', " ").replace('\r', ""),
            )
            .env("GIT_AUTHOR_DATE", record.author.date_env())
            .env(
                "GIT_COMMITTER_NAME",
                record.committer.name.replace('\n', " ").replace('\r', ""),
            )
            .env(
                "GIT_COMMITTER_EMAIL",
                record.committer.email.replace('\n', " ").replace('\r', ""),
            )
            .env("GIT_COMMITTER_DATE", record.committer.date_env())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd
    }

    /// Pipe the message into a prepared commit-tree command, cache the result
    /// in the export map, and return the new commit sha.
    fn finish_commit(&self, mut cmd: Command, message: &str, id: &str) -> Result<String> {
        let mut child = cmd.spawn().context("failed to run git commit-tree")?;
        use std::io::Write;
        child
            .stdin
            .take()
            .context("commit-tree has no stdin")?
            .write_all(message.as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "commit-tree failed for change {id}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let sha = String::from_utf8(output.stdout)?.trim().to_string();
        std::fs::create_dir_all(self.root.join("export"))?;
        std::fs::write(self.export_map_path(id), &sha)?;
        Ok(sha)
    }

    /// Append one withholding decision to `.oot/export-log.jsonl`. This is
    /// the audit trail for filtered exports: it says exactly what was left
    /// out of an export and why, before anyone pushes anything anywhere.
    fn log_withheld(&self, id: &str, source_sha: Option<&str>, reason: &str) -> Result<()> {
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": "withheld-change",
            "change": id,
            "source_sha": source_sha,
            "reason": reason,
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(EXPORT_LOG))?;
        writeln!(f, "{entry}")?;
        Ok(())
    }

    /// Record that a whole branch was omitted from an export because its head
    /// history was entirely withheld.
    pub fn log_branch_omitted(&self, branch: &str, head_id: &str) -> Result<()> {
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": "branch-omitted",
            "branch": branch,
            "change": head_id,
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(EXPORT_LOG))?;
        writeln!(f, "{entry}")?;
        Ok(())
    }

    /// Update a branch ref in the exported repository to point at `sha`.
    pub fn point_ref(&self, out_repo: &Path, branch: &str, sha: &str) -> Result<()> {
        run(Command::new("git")
            .args(["--git-dir"])
            .arg(out_repo.join(".git"))
            .args(["update-ref", &format!("refs/heads/{branch}"), sha]))?;
        Ok(())
    }

    fn export_map_path(&self, id: &str) -> PathBuf {
        self.root.join("export").join(id)
    }

    /// Whether a commit object with this sha exists in the store's odb.
    fn commit_object_exists(&self, sha: &str) -> Result<bool> {
        Ok(Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .output()
            .context("failed to probe the store's object database")?
            .status
            .success())
    }

    /// The exported commit sha for a change id, if this store has exported before.
    pub fn exported_sha(&self, id: &str) -> Result<Option<String>> {
        let f = self.export_map_path(id);
        if !f.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(f)?.trim().to_string()))
    }

    /// Discover all change IDs reachable from the given roots and all branch refs.
    /// Fails loudly if any reachable change record cannot be read.
    pub fn reachable_changes(&self, extra_roots: &[String]) -> Result<HashSet<String>> {
        let mut reachable = HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        for root in extra_roots {
            if !root.is_empty() {
                queue.push_back(root.clone());
            }
        }

        for (_, head_id) in self.refs()? {
            queue.push_back(head_id);
        }

        while let Some(id) = queue.pop_front() {
            if reachable.insert(id.clone()) {
                let rec = self.get_change(&id)?;
                for parent in rec.parents {
                    if !reachable.contains(&parent) {
                        queue.push_back(parent);
                    }
                }
            }
        }

        Ok(reachable)
    }

    /// Garbage collect and prune unreferenced changes, dockets, mappings, and odb objects.
    pub fn gc(
        &self,
        extra_roots: &[String],
        expire_cutoff: Option<std::time::SystemTime>,
        force: bool,
        dry_run: bool,
    ) -> Result<GcStats> {
        let live = self.reachable_changes(extra_roots)?;
        let mut stats = GcStats {
            live_changes: live.len(),
            ..Default::default()
        };

        let is_expired = |path: &Path| -> bool {
            if force || expire_cutoff.is_none() {
                return true;
            }
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(mtime) = meta.modified() {
                    if let Some(cutoff) = expire_cutoff {
                        return mtime <= cutoff;
                    }
                }
            }
            false
        };

        // Pre-scan unreferenced changes that are NOT expired, and pin all their DAG ancestors
        let mut pinned_by_unexpired = HashSet::new();
        let changes_dir = self.root.join(CHANGES_DIR);
        if changes_dir.exists() {
            let mut unexpired_queue = std::collections::VecDeque::new();
            for entry in std::fs::read_dir(&changes_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                        if !live.contains(id) && !is_expired(&path) {
                            unexpired_queue.push_back(id.to_string());
                        }
                    }
                }
            }
            while let Some(id) = unexpired_queue.pop_front() {
                if pinned_by_unexpired.insert(id.clone()) {
                    if let Ok(rec) = self.get_change(&id) {
                        for p in rec.parents {
                            if !live.contains(&p) && !pinned_by_unexpired.contains(&p) {
                                unexpired_queue.push_back(p);
                            }
                        }
                    }
                }
            }
        }

        // 1. Changes directory
        if changes_dir.exists() {
            for entry in std::fs::read_dir(&changes_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                        if !live.contains(file_name)
                            && !pinned_by_unexpired.contains(file_name)
                            && is_expired(&path)
                        {
                            if !dry_run {
                                if std::fs::remove_file(&path).is_ok() {
                                    stats.changes_pruned += 1;
                                }
                            } else {
                                stats.changes_pruned += 1;
                            }
                        }
                    }
                }
            }
        }

        // 2. Dockets directory
        let dockets_dir = self.root.join("dockets");
        if dockets_dir.exists() {
            for entry in std::fs::read_dir(&dockets_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                        if !live.contains(file_name)
                            && !pinned_by_unexpired.contains(file_name)
                            && is_expired(&path)
                        {
                            if !dry_run {
                                if std::fs::remove_file(&path).is_ok() {
                                    stats.dockets_pruned += 1;
                                }
                            } else {
                                stats.dockets_pruned += 1;
                            }
                        }
                    }
                }
            }
        }

        // 3. Map directory (commit sha -> change id)
        let map_dir = self.root.join(MAP_DIR);
        if map_dir.exists() {
            for entry in std::fs::read_dir(&map_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    if let Ok(target_id) = std::fs::read_to_string(&path) {
                        let target_id = target_id.trim();
                        if !live.contains(target_id)
                            && !pinned_by_unexpired.contains(target_id)
                            && is_expired(&path)
                        {
                            if !dry_run {
                                if std::fs::remove_file(&path).is_ok() {
                                    stats.map_pruned += 1;
                                }
                            } else {
                                stats.map_pruned += 1;
                            }
                        }
                    }
                }
            }
        }

        // 4. Export directory (change id -> exported commit sha)
        let export_dir = self.root.join("export");
        if export_dir.exists() {
            for entry in std::fs::read_dir(&export_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    if let Some(file_name) = path.file_name().and_then(|s| s.to_str()) {
                        if file_name != "policy-key"
                            && file_name != EXPORT_LOG
                            && !live.contains(file_name)
                            && !pinned_by_unexpired.contains(file_name)
                            && is_expired(&path)
                        {
                            if !dry_run {
                                if std::fs::remove_file(&path).is_ok() {
                                    stats.export_pruned += 1;
                                }
                            } else {
                                stats.export_pruned += 1;
                            }
                        }
                    }
                }
            }
        }

        // 5. Rewrite .index retaining only changes that still exist on disk
        if !dry_run && stats.changes_pruned > 0 {
            if let Ok(old_index) = self.index() {
                let kept_ordered: Vec<String> = old_index
                    .into_iter()
                    .filter(|id| changes_dir.join(format!("{id}.json")).exists())
                    .collect();
                let tmp_index = self.root.join(".index.tmp");
                let mut content = String::new();
                for id in kept_ordered {
                    content.push_str(&id);
                    content.push('\n');
                }
                std::fs::write(&tmp_index, content)?;
                std::fs::rename(tmp_index, self.root.join(".index"))?;
            }
        }

        // 6. Object DB compaction and prune: protect trees and commits for ALL preserved changes
        if !dry_run && stats.changes_pruned > 0 {
            // Clean up any preexisting stale gc refs
            let existing_gc = Command::new("git")
                .args(["--git-dir"])
                .arg(self.git_dir())
                .args(["for-each-ref", "--format=%(refname)", "refs/oot/gc"])
                .output();
            if let Ok(out) = existing_gc {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let r = line.trim();
                    if !r.is_empty() {
                        let _ = Command::new("git")
                            .args(["--git-dir"])
                            .arg(self.git_dir())
                            .args(["update-ref", "-d", r])
                            .output();
                    }
                }
            }

            let mut gc_refs = Vec::new();
            let mut seen_refs = HashSet::new();

            let mut remaining_ids = Vec::new();
            if changes_dir.exists() {
                for entry in std::fs::read_dir(&changes_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                            remaining_ids.push(id.to_string());
                        }
                    }
                }
            }

            for id in &remaining_ids {
                let record = self.get_change(id)?;
                if record.tree != EMPTY_TREE {
                    let ref_path = format!("refs/oot/gc/{}", record.tree);
                    if seen_refs.insert(ref_path.clone()) {
                        let out = Command::new("git")
                            .args(["--git-dir"])
                            .arg(self.git_dir())
                            .args(["update-ref", &ref_path, &record.tree])
                            .output()
                            .context("failed to write protective GC tree ref")?;
                        if !out.status.success() {
                            bail!(
                                "git update-ref failed for {ref_path}: {}",
                                String::from_utf8_lossy(&out.stderr).trim()
                            );
                        }
                        gc_refs.push(ref_path);
                    }
                }

                if let Some(src) = &record.source_sha {
                    let src_ref = format!("refs/oot/gc/{}", src);
                    if seen_refs.insert(src_ref.clone()) {
                        let out = Command::new("git")
                            .args(["--git-dir"])
                            .arg(self.git_dir())
                            .args(["update-ref", &src_ref, src])
                            .output()
                            .context("failed to write protective GC source ref")?;
                        if !out.status.success() {
                            bail!(
                                "git update-ref failed for {src_ref}: {}",
                                String::from_utf8_lossy(&out.stderr).trim()
                            );
                        }
                        gc_refs.push(src_ref);
                    }
                }

                if let Some(exported_sha) = self.exported_sha(id)? {
                    let exp_ref = format!("refs/oot/gc/{}", exported_sha);
                    if seen_refs.insert(exp_ref.clone()) {
                        let out = Command::new("git")
                            .args(["--git-dir"])
                            .arg(self.git_dir())
                            .args(["update-ref", &exp_ref, &exported_sha])
                            .output()
                            .context("failed to write protective GC export commit ref")?;
                        if !out.status.success() {
                            bail!(
                                "git update-ref failed for {exp_ref}: {}",
                                String::from_utf8_lossy(&out.stderr).trim()
                            );
                        }
                        gc_refs.push(exp_ref);
                    }
                }
            }

            let repack_out = Command::new("git")
                .args(["--git-dir"])
                .arg(self.git_dir())
                .args(["repack", "-a", "-d"])
                .output()
                .context("failed to repack git odb during gc")?;
            if !repack_out.status.success() {
                bail!(
                    "git repack failed during gc: {}",
                    String::from_utf8_lossy(&repack_out.stderr).trim()
                );
            }

            let prune_out = Command::new("git")
                .args(["--git-dir"])
                .arg(self.git_dir())
                .args(["prune", "--expire=now"])
                .output()
                .context("failed to prune git odb during gc")?;
            if !prune_out.status.success() {
                bail!(
                    "git prune failed during gc: {}",
                    String::from_utf8_lossy(&prune_out.stderr).trim()
                );
            }

            for ref_path in gc_refs {
                let _ = Command::new("git")
                    .args(["--git-dir"])
                    .arg(self.git_dir())
                    .args(["update-ref", "-d", &ref_path])
                    .output();
            }
        }

        Ok(stats)
    }
}

/// Summary statistics from a store garbage collection and pruning run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcStats {
    pub live_changes: usize,
    pub changes_pruned: usize,
    pub dockets_pruned: usize,
    pub map_pruned: usize,
    pub export_pruned: usize,
}

/// One commit as read from a source repository, before becoming a record.
#[derive(Debug, Clone)]
pub struct RawCommit {
    pub sha: String,
    pub tree: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub author_iso: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_time: i64,
    pub committer_iso: String,
    pub message: String,
}

impl RawCommit {
    /// Pull the next non-empty NUL-separated field or fail loudly.
    fn required<'a, I: Iterator<Item = &'a str>>(parts: &mut I, what: &str) -> Result<String> {
        parts
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("commit record missing field '{what}'"))
    }

    fn parse(record: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(record)
            .map_err(|_| anyhow!("commit record is not valid UTF-8"))?
            .to_string();
        let mut parts = text.split('\0');
        let sha = Self::required(&mut parts, "sha")?;
        let tree = Self::required(&mut parts, "tree")?;
        // The root commit legitimately has an empty parent list.
        let parents: Vec<String> = parts
            .next()
            .unwrap_or("")
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let author_name = Self::required(&mut parts, "author name")?;
        let author_email = Self::required(&mut parts, "author email")?;
        let author_time = Self::required(&mut parts, "author time")?
            .parse()
            .context("bad author timestamp")?;
        let author_iso = Self::required(&mut parts, "author date")?;
        let committer_name = Self::required(&mut parts, "committer name")?;
        let committer_email = Self::required(&mut parts, "committer email")?;
        let committer_time = Self::required(&mut parts, "committer time")?
            .parse()
            .context("bad committer timestamp")?;
        let committer_iso = Self::required(&mut parts, "committer date")?;
        let message = parts.next().unwrap_or("").to_string();

        Ok(Self {
            sha,
            tree,
            parents,
            author_name,
            author_email,
            author_time,
            author_iso,
            committer_name,
            committer_email,
            committer_time,
            committer_iso,
            message,
        })
    }
}

/// Extract the raw timezone offset (e.g. `+0530`) from a git ISO-8601 date
/// like `2026-08-22T10:00:00+05:30`. UTC may arrive as a bare `Z` suffix
/// (runner clocks are UTC); historical offsets can be exotic; those fail
/// loudly rather than silently rewriting dates.
pub fn parse_offset(iso: &str) -> Result<String> {
    if iso.ends_with(['Z', 'z']) {
        return Ok("+0000".to_string());
    }
    let tail = iso
        .rsplit(['+', '-'])
        .next()
        .context("date missing timezone offset")?;
    if tail.len() >= iso.len() {
        bail!("date missing timezone offset: '{iso}'");
    }
    let sign_start = iso.len() - tail.len() - 1;
    let sign = &iso[sign_start..sign_start + 1];
    let digits: String = tail.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.len() {
        4 => Ok(format!("{sign}{digits}")),
        _ => bail!("unsupported timezone offset in date '{iso}'"),
    }
}

/// Seconds since the Unix epoch; used for record timestamps and log entries.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// UTC calendar date of `epoch` shifted into the identity's timezone, as
/// `YYYY-MM-DD`. Pure arithmetic; no date libraries in the tree.
pub fn format_date(identity: &Identity) -> String {
    let sign = if identity.offset.starts_with('-') {
        -1
    } else {
        1
    };
    let digits: String = identity
        .offset
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    let offset_secs: i64 = if digits.len() == 4 {
        sign * (digits[..2].parse::<i64>().unwrap_or(0) * 3600
            + digits[2..].parse::<i64>().unwrap_or(0) * 60)
    } else {
        0
    };
    let days = (identity.time + offset_secs).div_euclid(86400);
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn encode_branch(branch: &str) -> String {
    branch.replace('%', "%25").replace('/', "%2F")
}

fn decode_branch(raw: &str) -> String {
    // Scan char-by-char to avoid double-decode issues (e.g. "%252F").
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i..].starts_with("%2F") {
            out.push('/');
            i += 3;
        } else if raw[i..].starts_with("%25") {
            out.push('%');
            i += 3;
        } else {
            let c = raw[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

pub fn validate_tree_path(path: &str) -> Result<()> {
    if path.contains('\0') {
        bail!("invalid path in tree: '{}': contains NUL byte", path);
    }
    if path.is_empty() {
        bail!("invalid path in tree: '{}': empty path", path);
    }
    if path.starts_with('/') {
        bail!("invalid path in tree: '{}': absolute path", path);
    }
    if path.contains("//") {
        bail!(
            "invalid path in tree: '{}': empty path component (//)",
            path
        );
    }
    if path.split('/').any(|c| c.is_empty()) {
        bail!("invalid path in tree: '{}': empty path component", path);
    }
    use std::path::{Component, Path};
    let p = Path::new(path);
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                bail!("invalid path in tree: '{}': contains '..' component", path);
            }
            Component::CurDir => {
                bail!("invalid path in tree: '{}': contains '.' component", path);
            }
            Component::RootDir => {
                bail!("invalid path in tree: '{}': absolute path component", path);
            }
            Component::Prefix(_) => {
                bail!("invalid path in tree: '{}': prefix component", path);
            }
            Component::Normal(os) => {
                let name = os.to_string_lossy();
                let lower = name.to_ascii_lowercase();
                if lower == ".git" || lower == ".oot" || lower == ".jj" {
                    bail!(
                        "invalid path in tree: '{}': cannot write into vcs directory '{}'",
                        path,
                        name
                    );
                }
            }
        }
    }
    Ok(())
}

fn run(cmd: &mut Command) -> Result<()> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {}", cmd.get_program().to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_offset() {
        assert_eq!(parse_offset("2026-08-22T10:00:00+05:30").unwrap(), "+0530");
        assert_eq!(parse_offset("2026-08-22T10:00:00-08:00").unwrap(), "-0800");
        assert_eq!(parse_offset("1970-01-01T00:00:00+00:00").unwrap(), "+0000");
        // UTC runners render zero offsets as a bare Z suffix.
        assert_eq!(parse_offset("2026-08-22T10:00:00Z").unwrap(), "+0000");
        assert!(parse_offset("no offset here").is_err());
    }

    #[test]
    fn test_raw_commit_parse_roundtrip() {
        let record = b"abc123\x00tree999\x00def456\x00Kriday\x00k@oot.dev\x001700000000\x002023-11-14T22:13:20+05:30\x00Kriday\x00k@oot.dev\x001700000001\x002023-11-14T22:13:21+05:30\x00Add feature\n\nBody line.\n";
        let raw = RawCommit::parse(record).unwrap();
        assert_eq!(raw.sha, "abc123");
        assert_eq!(raw.tree, "tree999");
        assert_eq!(raw.parents, vec!["def456"]);
        assert_eq!(raw.author_time, 1_700_000_000);
        assert_eq!(raw.message, "Add feature\n\nBody line.\n");
    }

    #[test]
    fn test_raw_commit_rejects_truncation() {
        let short = b"abc123\x00tree999";
        assert!(RawCommit::parse(short).is_err());
    }

    #[test]
    fn test_store_init_open_and_record_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("oot-store-test-{}", std::process::id()));
        let project = tmp.join("proj");
        std::fs::create_dir_all(&project).unwrap();

        Store::init(&project).unwrap();
        assert!(Store::init(&project).is_err(), "double init must fail");

        // Discovery walks upward from a nested directory.
        let nested = project.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let store = Store::open(&nested).expect("open from nested dir");

        let raw = RawCommit {
            sha: "aaa".into(),
            tree: "ttt".into(),
            parents: vec![],
            author_name: "K".into(),
            author_email: "k@oot.dev".into(),
            author_time: 1_700_000_000,
            author_iso: "2023-11-14T22:13:20+05:30".into(),
            committer_name: "K".into(),
            committer_email: "k@oot.dev".into(),
            committer_time: 1_700_000_000,
            committer_iso: "2023-11-14T22:13:20+05:30".into(),
            message: "msg\n".into(),
        };

        let id = store.put_commit(&raw).unwrap();
        assert_eq!(
            store.change_for_commit("aaa").unwrap().as_deref(),
            Some(id.as_str())
        );
        assert_eq!(
            store.put_commit(&raw).unwrap(),
            id,
            "import must be idempotent"
        );

        let rec = store.get_change(&id).unwrap();
        assert_eq!(rec.tree, "ttt");
        assert_eq!(rec.author.offset, "+0530");
        assert_eq!(rec.message, "msg\n");

        store.index_push(&id).unwrap();
        store.index_push(&id).unwrap();
        assert_eq!(
            store.index().unwrap(),
            vec![id.clone()],
            "index must dedupe"
        );

        store.set_ref("feature/one", &id).unwrap();
        assert_eq!(
            store.refs().unwrap(),
            vec![("feature/one".to_string(), id.clone())]
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_snapshot_from_tree_and_read_blob_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("oot-snap-test-{}", std::process::id()));
        let project = tmp.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let store = Store::init(&project).unwrap();

        let files = vec![
            WorkFile {
                path: "lib.rs".into(),
                contents: b"pub fn a() {}\n".to_vec(),
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
        assert_eq!(snap.files["lib.rs"], b"pub fn a() {}\n".to_vec());
        assert_eq!(
            snap.files["deep/nested/bin.dat"],
            vec![0x00, 0xff, 0x7f, 0x80]
        );

        let (sha, _) = store
            .tree_files(&tree)
            .unwrap()
            .get("run.sh")
            .unwrap()
            .clone();
        assert_eq!(store.read_blob(&sha).unwrap(), b"#!/bin/sh\n".to_vec());
        assert!(store.read_blob("does-not-exist").is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_resolve_change_exact_prefix_and_ambiguity() {
        let tmp = std::env::temp_dir().join(format!("oot-resolve-test-{}", std::process::id()));
        let project = tmp.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let store = Store::init(&project).unwrap();

        // Two crafted ids sharing a long prefix; resolve_change works on
        // stored filenames, so hand-written records are a deterministic fixture.
        let changes = store.path().join("changes");
        for tail in ["1", "2"] {
            let id = format!("aaaa00000000000000000000000000000000000{tail}");
            let record = ChangeRecord {
                parents: vec![],
                tree: format!("tree-{tail}"),
                author: Identity {
                    name: "K".into(),
                    email: "k@oot.dev".into(),
                    time: 0,
                    offset: "+0000".into(),
                },
                committer: Identity {
                    name: "K".into(),
                    email: "k@oot.dev".into(),
                    time: 0,
                    offset: "+0000".into(),
                },
                message: "crafted\n".into(),
                source_sha: None,
            };
            std::fs::write(
                changes.join(format!("{id}.json")),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        }

        assert_eq!(
            store
                .resolve_change("aaaa000000000000000000000000000000000001")
                .unwrap(),
            "aaaa000000000000000000000000000000000001",
            "exact id wins"
        );
        assert_eq!(
            store
                .resolve_change("aaaa000000000000000000000000000000000002")
                .unwrap(),
            "aaaa000000000000000000000000000000000002"
        );

        let ambiguous = store.resolve_change("aaaa").unwrap_err().to_string();
        assert!(
            ambiguous.contains("ambiguous change prefix 'aaaa'"),
            "{ambiguous}"
        );
        assert!(
            ambiguous.contains("aaaa000000000000000000000000000000000001"),
            "{ambiguous}"
        );
        assert!(
            ambiguous.contains("aaaa000000000000000000000000000000000002"),
            "{ambiguous}"
        );

        let missing = store.resolve_change("bbbb").unwrap_err().to_string();
        assert!(missing.contains("no change matching 'bbbb'"), "{missing}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_identity_date_env_preserves_offset() {
        let id = Identity {
            name: "K".into(),
            email: "k@oot.dev".into(),
            time: 1_700_000_000,
            offset: "+0530".into(),
        };
        assert_eq!(id.date_env(), "1700000000 +0530");
    }

    #[test]
    fn test_format_date_applies_offset() {
        let mk = |time: i64, offset: &str| Identity {
            name: "K".into(),
            email: "k@oot.dev".into(),
            time,
            offset: offset.into(),
        };
        // Same instant: +0530 is already the next day vs UTC.
        assert_eq!(format_date(&mk(1_700_000_000, "+0530")), "2023-11-15");
        assert_eq!(format_date(&mk(1_700_000_000, "-0800")), "2023-11-14");
        assert_eq!(format_date(&mk(1_700_000_000, "+0000")), "2023-11-14");
        // Exotic historical offset still parses.
        assert_eq!(format_date(&mk(1, "+0045")), "1970-01-01");
    }
}
