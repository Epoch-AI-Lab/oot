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
    pub fn head_id(&self, branch: &str) -> Result<Option<String>> {
        let safe = branch.replace('/', "__");
        let f = self.root.join(REFS_DIR).join(safe);
        if !f.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(f)?.trim().to_string()))
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
        let safe = branch.replace('/', "__");
        std::fs::write(self.root.join(REFS_DIR).join(safe), id)?;
        Ok(())
    }

    /// Read all recorded branches as (branch, head change id).
    pub fn refs(&self) -> Result<Vec<(String, String)>> {
        let dir = self.root.join(REFS_DIR);
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().replace("__", "/");
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
    /// Filtering any part of history disables the identity fast path for the
    /// whole export — rebuilt history must be internally consistent, so no
    /// original objects are reused alongside it.
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

        let filtering = policy.is_some_and(|p| !p.private_paths.is_empty());

        // Export mappings are only valid for the policy they were produced
        // under: a filtered export's shas mean nothing to an unfiltered one
        // and vice versa. A changed policy wipes the cache before anything
        // can silently mix decisions from two regimes.
        let filter_key = match policy {
            Some(p) if filtering => p.private_paths.join("\u{1f}"),
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
                exported.push((id, sha));
                continue;
            }

            // Identity fast path: reuse the original commit object when the
            // whole ancestry below is byte-exact, so signatures and other
            // extra headers survive without reconstruction. Never taken while
            // filtering: reused objects must not mix with rebuilt history.
            if !filtering {
                if let Some(orig) = &record.source_sha {
                    let parents_exact = record.parents.iter().all(|p| {
                        sha_of
                            .get(p)
                            .is_some_and(|e| source_sha_of.get(p) == Some(e))
                    });
                    if parents_exact && self.commit_object_exists(orig)? {
                        std::fs::write(self.export_map_path(&id), orig)?;
                        sha_of.insert(id.clone(), orig.clone());
                        source_sha_of.insert(id.clone(), orig.clone());
                        exported.push((id.clone(), orig.clone()));
                        continue;
                    }
                }
            }

            // Filtered path: withhold tainted changes, strip kept trees,
            // remap parents to nearest kept ancestors.
            if filtering {
                if let Some(reason) = withheld.get(&id) {
                    self.log_withheld(&id, record.source_sha.as_deref(), reason)?;
                    continue;
                }
                let pol = policy.unwrap();
                let tree = self.strip_tree(&record.tree, pol, "")?;
                let mut parent_shas: Vec<String> = Vec::new();
                for p in &record.parents {
                    for anc in self.nearest_kept(p, &sha_of)? {
                        if !parent_shas.contains(&anc) {
                            parent_shas.push(anc);
                        }
                    }
                }

                // A change left empty by stripping carries nothing over its
                // kept ancestry: skip it rather than ship an empty commit.
                // Root commits are always kept so children retain a root.
                let empty_over_ancestry = !parent_shas.is_empty()
                    && parent_shas.iter().all(|p| tree_of.get(p) == Some(&tree));
                if empty_over_ancestry {
                    let why = "empty after private-path stripping".to_string();
                    self.log_withheld(&id, record.source_sha.as_deref(), &why)?;
                    continue;
                }

                let mut cmd = self.commit_tree_cmd(&tree, &record);
                for p in &parent_shas {
                    cmd.args(["-p", p]);
                }
                let sha = self.finish_commit(cmd, &record.message, &id)?;
                tree_of.insert(sha.clone(), tree);
                sha_of.insert(id.clone(), sha.clone());
                exported.push((id.clone(), sha));
                continue;
            }

            let missing = record
                .parents
                .iter()
                .filter(|p| !sha_of.contains_key(*p))
                .count();
            if missing > 0 {
                bail!("change {id} references unexported parents");
            }
            let mut cmd = self.commit_tree_cmd(&record.tree, &record);
            for p in &record.parents {
                cmd.args(["-p", &sha_of[p]]);
            }
            let sha = self.finish_commit(cmd, &record.message, &id)?;
            sha_of.insert(id.clone(), sha.clone());
            exported.push((id, sha));
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
    /// changes that were withheld or skipped. Order-stable and deduped.
    fn nearest_kept(&self, id: &str, sha_of: &HashMap<String, String>) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut queue = vec![id.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(id.to_string());
        while let Some(cur) = queue.pop() {
            if let Some(sha) = sha_of.get(&cur) {
                if !out.contains(sha) {
                    out.push(sha.clone());
                }
                continue;
            }
            for p in self.get_change(&cur)?.parents {
                if seen.insert(p.clone()) {
                    queue.push(p);
                }
            }
        }
        Ok(out)
    }

    /// The exported head commit for a branch whose head change is `head_id`,
    /// walking up through withheld/skipped changes. `None` means the branch's
    /// entire history was withheld and the ref should be omitted.
    pub fn branch_head_sha(&self, head_id: &str) -> Result<Option<String>> {
        let mut queue = vec![head_id.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(head_id.to_string());
        while let Some(cur) = queue.pop() {
            if let Some(sha) = self.exported_sha(&cur)? {
                return Ok(Some(sha));
            }
            for p in self.get_change(&cur)?.parents {
                if seen.insert(p.clone()) {
                    queue.push(p);
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
                // Gitlinks (submodules) are commit refs we never rewrite.
                "commit" => lines.push(record.to_string()),
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
                    lines.push(format!("{mode} tree {sub}\t{name}"));
                }
                other => bail!("unexpected entry kind '{other}' in tree {tree}"),
            }
        }

        if !changed {
            return Ok(tree.to_string());
        }

        use std::io::Write;
        let mut child = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .arg("mktree")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to run git mktree")?;
        child
            .stdin
            .take()
            .context("mktree has no stdin")?
            .write_all(lines.join("\n").as_bytes())?;
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
            .env("GIT_AUTHOR_NAME", &record.author.name)
            .env("GIT_AUTHOR_EMAIL", &record.author.email)
            .env("GIT_AUTHOR_DATE", record.author.date_env())
            .env("GIT_COMMITTER_NAME", &record.committer.name)
            .env("GIT_COMMITTER_EMAIL", &record.committer.email)
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
    fn test_identity_date_env_preserves_offset() {
        let id = Identity {
            name: "K".into(),
            email: "k@oot.dev".into(),
            time: 1_700_000_000,
            offset: "+0530".into(),
        };
        assert_eq!(id.date_env(), "1700000000 +0530");
    }
}
