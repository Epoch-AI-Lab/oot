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
const TAGS_DIR: &str = "tags";
const TAGMETA_DIR: &str = "tagmeta";
const EXPORT_LOG: &str = "export-log.jsonl";
/// Git's well-known empty tree; used to diff root commits against nothing.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Git's well-known empty blob SHA.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

/// An acquired advisory lock on the store. Released when dropped.
#[derive(Debug)]
pub struct StoreLock {
    path: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

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
        std::fs::create_dir_all(oot.join(TAGS_DIR))?;
        std::fs::create_dir_all(oot.join(TAGMETA_DIR))?;
        std::fs::create_dir_all(oot.join("export"))?;
        std::fs::write(oot.join("HEAD"), "ref: refs/heads/main\n")?;
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

    /// Acquire an exclusive advisory lock on the store.
    pub fn lock(&self) -> Result<StoreLock> {
        let lock_path = self.root.join("lock");
        let start = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => {
                    use std::io::Write;
                    let _ = writeln!(&file, "{}", std::process::id());
                    return Ok(StoreLock { path: lock_path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Break stale lock if older than 10 seconds
                    if let Ok(meta) = std::fs::metadata(&lock_path) {
                        if let Ok(mtime) = meta.modified() {
                            if let Ok(elapsed) = mtime.elapsed() {
                                if elapsed > std::time::Duration::from_secs(10) {
                                    let _ = std::fs::remove_file(&lock_path);
                                    continue;
                                }
                            }
                        }
                    }
                    if start.elapsed() > std::time::Duration::from_secs(5) {
                        bail!("timed out waiting for store lock on {:?}", lock_path);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Read the current active branch from `.oot/HEAD`.
    pub fn get_head_branch(&self) -> Result<Option<String>> {
        let head_path = self.root.join("HEAD");
        if !head_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(head_path)?;
        let trimmed = content.trim();
        if let Some(rest) = trimmed.strip_prefix("ref: refs/heads/") {
            Ok(Some(rest.to_string()))
        } else if !trimmed.is_empty() {
            Ok(Some(trimmed.to_string()))
        } else {
            Ok(None)
        }
    }

    /// Set the current active branch in `.oot/HEAD`.
    pub fn set_head_branch(&self, branch: &str) -> Result<()> {
        let head_path = self.root.join("HEAD");
        let content = format!("ref: refs/heads/{branch}\n");
        std::fs::write(head_path, content)?;
        Ok(())
    }

    /// Collect all `(path, blob_sha)` pairs in `tree` recursively from the odb.
    pub fn collect_tree_blobs(&self, tree: &str, prefix: &str) -> Result<Vec<(String, String)>> {
        if tree == EMPTY_TREE {
            return Ok(Vec::new());
        }
        let listed = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["ls-tree", "-r", "-z", tree])
            .output()
            .context("failed to list tree objects in store")?;
        if !listed.status.success() {
            bail!("git ls-tree failed for {tree}");
        }
        let mut out = Vec::new();
        for entry in listed.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()) {
            let record = String::from_utf8_lossy(entry);
            let (meta, path) = match record.split_once('\t') {
                Some(p) => p,
                None => continue,
            };
            let parts: Vec<&str> = meta.split_whitespace().collect();
            if parts.len() >= 3 && parts[1] == "blob" {
                out.push((format!("{prefix}{path}"), parts[2].to_string()));
            }
        }
        Ok(out)
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

    /// Whether the store contains a ref for `branch`.
    pub fn has_ref(&self, branch: &str) -> Result<bool> {
        self.head_id(branch).map(|opt| opt.is_some())
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

    /// Fetch all tags from a source repository into the store's odb and
    /// return their short names. Annotated tags peel to their target commit
    /// on import; tag objects and messages are not preserved, export writes
    /// lightweight refs. Branch objects must already be fetched.
    pub fn fetch_tags(&self, source_repo: &Path) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["for-each-ref", "--format=%(refname:short)", "refs/tags"])
            .current_dir(source_repo)
            .output()
            .context("failed to list tags in source repository")?;
        if !output.status.success() {
            bail!("not a valid git repository: {}", source_repo.display());
        }
        let tags: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();

        if !tags.is_empty() {
            run(Command::new("git")
                .args(["--git-dir"])
                .arg(self.git_dir())
                .args(["fetch", "--quiet"])
                .arg(source_repo)
                .args(["+refs/tags/*:refs/oot/source-tags/*"]))?;
        }

        Ok(tags)
    }

    /// Peel a source tag to its target commit sha. The full ref path plus
    /// `^{commit}` resolves annotated tags through to the commit, keeps a
    /// tag named like a branch from misresolving, and fails on non-commit
    /// targets instead of poisoning the commit map.
    pub fn peel_tag(&self, source_repo: &Path, tag: &str) -> Result<String> {
        let reference = format!("refs/tags/{tag}^{{commit}}");
        let output = Command::new("git")
            .args(["rev-parse", "--verify", "--end-of-options", &reference])
            .current_dir(source_repo)
            .output()
            .context("failed to peel tag in source repository")?;
        if !output.status.success() {
            bail!(
                "failed to resolve tag '{tag}' to a commit: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Whether `tag` is a safe refname for `refs/tags/<tag>`.
    pub fn valid_tag_ref(tag: &str) -> bool {
        if tag.is_empty() {
            return false;
        }
        Command::new("git")
            .args(["check-ref-format", &format!("refs/tags/{tag}")])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Record the head change id for a tag. Names share the branch
    /// percent-encoding so `v1/rc1` survives as one file.
    pub fn set_tag(&self, tag: &str, id: &str) -> Result<()> {
        std::fs::create_dir_all(self.root.join(TAGS_DIR))?;
        let safe = encode_branch(tag);
        std::fs::write(self.root.join(TAGS_DIR).join(safe), id)?;
        Ok(())
    }

    /// Record the original annotated tag's identity — tagger, message,
    /// whether it carried a signature — so export can recreate the tag at
    /// a rebuilt target with the same content and a fresh signature.
    pub fn set_tag_meta(&self, tag: &str, meta: &TagMeta) -> Result<()> {
        std::fs::create_dir_all(self.root.join(TAGMETA_DIR))?;
        let safe = encode_branch(tag);
        let path = self.root.join(TAGMETA_DIR).join(safe);
        std::fs::write(path, serde_json::to_string(meta)?)?;
        Ok(())
    }

    /// Forget a tag's metadata: its replacement is lightweight (or was
    /// re-imported as lightweight), so old identity must not leak into
    /// the next export.
    pub fn clear_tag_meta(&self, tag: &str) -> Result<()> {
        let path = self.root.join(TAGMETA_DIR).join(encode_branch(tag));
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// The original annotated tag's identity, if import captured one.
    pub fn tag_meta(&self, tag: &str) -> Result<Option<TagMeta>> {
        let path = self.root.join(TAGMETA_DIR).join(encode_branch(tag));
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Read an annotated tag's identity straight from the source repo:
    /// tagger, message, and whether the message carries a signature.
    /// Lightweight tags have no tag object and come back as None.
    pub fn capture_tag_meta(&self, source_repo: &Path, tag: &str) -> Result<Option<TagMeta>> {
        let kind = Command::new("git")
            .args(["cat-file", "-t", &format!("refs/tags/{tag}")])
            .current_dir(source_repo)
            .output()
            .context("failed to inspect tags in source repository")?;
        if !kind.status.success() || String::from_utf8_lossy(&kind.stdout).trim() != "tag" {
            return Ok(None);
        }
        let body = Command::new("git")
            .args(["cat-file", "tag", &format!("refs/tags/{tag}")])
            .current_dir(source_repo)
            .output()
            .context("failed to read tags in source repository")?;
        if !body.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&body.stdout);
        let (headers, message) = text.split_once("\n\n").unwrap_or((&text, ""));
        let tagger = headers
            .lines()
            .find(|l| l.starts_with("tagger "))
            .and_then(|l| {
                let rest = l.strip_prefix("tagger ")?;
                let open = rest.find('<')?;
                let close = rest.find('>')?;
                let name = rest[..open].trim();
                let email = rest[open + 1..close].trim();
                Some((name.to_string(), email.to_string()))
            });
        Ok(Some(TagMeta {
            tagger_name: tagger.as_ref().map(|(n, _)| n.clone()),
            tagger_email: tagger.as_ref().map(|(_, e)| e.clone()),
            // A signed tag's signature lives in its message body: keep the
            // prose, drop the armor, or a recreated tag would carry the
            // stale signature text (and a fresh one after it) in its message.
            message: strip_signature_block(message),
            signed: message_has_signature(message),
        }))
    }

    /// Read all recorded tags as (tag, head change id).
    pub fn tags(&self) -> Result<Vec<(String, String)>> {
        let dir = self.root.join(TAGS_DIR);
        if !dir.exists() {
            return Ok(Vec::new());
        }
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

    /// Record that a tag was omitted from an export because its target
    /// history was entirely withheld.
    pub fn log_tag_omitted(&self, tag: &str, head_id: &str) -> Result<()> {
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": "tag-omitted",
            "tag": tag,
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

    /// Recreate an annotated tag at `sha` in the exported repo with the
    /// original tagger identity and message. With a signing key it ships
    /// freshly signed (the original signature covered the original bytes
    /// and cannot be copied); without one it is unsigned.
    fn recreate_tag(
        &self,
        out_repo: &Path,
        tag: &str,
        sha: &str,
        meta: &TagMeta,
        sign_key: Option<&str>,
    ) -> Result<()> {
        let mut cmd = Command::new("git");
        if let Some(key) = sign_key {
            cmd.arg("-c").arg(format!("user.signingkey={key}"));
        }
        cmd.args([
            "tag",
            if sign_key.is_some() { "-s" } else { "-a" },
            tag,
            "-m",
            &meta.message,
            sha,
        ]);
        if let (Some(name), Some(email)) = (&meta.tagger_name, &meta.tagger_email) {
            cmd.env("GIT_COMMITTER_NAME", name.replace('\n', " "))
                .env("GIT_COMMITTER_EMAIL", email.replace('\n', " "));
        }
        cmd.current_dir(out_repo);
        run(&mut cmd)?;
        Ok(())
    }

    /// Record that an exported tag's signature was replaced with a fresh
    /// one because its target commit was rebuilt.
    fn log_tag_resigned(&self, tag: &str, head_id: &str, key: &str) -> Result<()> {
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": "tag-resigned",
            "tag": tag,
            "change": head_id,
            "key": key,
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(EXPORT_LOG))?;
        writeln!(f, "{entry}")?;
        Ok(())
    }

    /// Update a tag ref in the exported repository to point at `sha`.
    pub fn point_tag(&self, out_repo: &Path, tag: &str, sha: &str) -> Result<()> {
        run(Command::new("git")
            .args(["--git-dir"])
            .arg(out_repo.join(".git"))
            .args(["update-ref", &format!("refs/tags/{tag}"), sha]))?;
        Ok(())
    }

    /// The original annotated tag object for `tag`, if import fetched one:
    /// (object sha, commit it peels to). Lightweight tags have no separate
    /// object, so they come back as None.
    fn source_tag_object(&self, tag: &str) -> Result<Option<(String, String)>> {
        let reference = format!("refs/oot/source-tags/{tag}");
        let resolve = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["rev-parse", "--verify", "--quiet", &reference])
            .output()
            .context("failed to probe the store's tag objects")?;
        if !resolve.status.success() {
            return Ok(None);
        }
        let obj = String::from_utf8_lossy(&resolve.stdout).trim().to_string();
        let kind = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["cat-file", "-t", &obj])
            .output()
            .context("failed to probe the store's tag objects")?;
        if !kind.status.success() || String::from_utf8_lossy(&kind.stdout).trim() != "tag" {
            return Ok(None);
        }
        let peel = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{obj}^{{commit}}"),
            ])
            .output()
            .context("failed to probe the store's tag objects")?;
        if !peel.status.success() {
            return Ok(None);
        }
        Ok(Some((
            obj,
            String::from_utf8_lossy(&peel.stdout).trim().to_string(),
        )))
    }

    /// Whether a tag object carries a signature. `git tag -s` puts the
    /// signature in the tag message body; check for the known armor headers
    /// there, not in the headers, so tagger text quoting armor is not a sig.
    fn tag_object_signed(&self, obj: &str) -> Result<bool> {
        let out = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["cat-file", "tag", obj])
            .output()
            .context("failed to read the store's tag object")?;
        if !out.status.success() {
            return Ok(false);
        }
        let body = String::from_utf8_lossy(&out.stdout);
        let message = body.split_once("\n\n").map(|(_, m)| m).unwrap_or(&body);
        Ok(message_has_signature(message))
    }

    /// Point an exported tag ref at the original tag object itself, so the
    /// annotated object (tagger, message, signature) survives the export
    /// byte-identically. The object resolves in the exported repo because
    /// replay attaches the store's odb.
    fn point_tag_object(&self, out_repo: &Path, tag: &str, obj: &str) -> Result<()> {
        run(Command::new("git")
            .args(["--git-dir"])
            .arg(out_repo.join(".git"))
            .args(["update-ref", &format!("refs/tags/{tag}"), obj]))?;
        Ok(())
    }

    /// Record that an exported tag lost its signature because its target
    /// commit was rebuilt (private-path stripping or parent remap): the
    /// signature covered the original object bytes and cannot ride along.
    fn log_tag_sig_dropped(&self, tag: &str, head_id: &str) -> Result<()> {
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": "tag-sig-dropped",
            "tag": tag,
            "change": head_id,
            "reason": "tag target was rebuilt; the signature covered the original bytes",
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(EXPORT_LOG))?;
        writeln!(f, "{entry}")?;
        Ok(())
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
                let resign = p
                    .resign_key_id
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default();
                format!(
                    "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                    p.private_paths.join(","),
                    p.private_branches.join(","),
                    p.embargo_until.as_deref().unwrap_or_default(),
                    resign
                )
            }
            _ => String::new(),
        };
        self.reset_export_cache_if_policy_changed(&filter_key)?;
        let sign_key: Option<String> = if filtering {
            policy
                .and_then(|p| p.resign_key_id.clone())
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
        } else {
            None
        };

        // Taint pass: decide once, up front, which changes touch private paths,
        // and collect all private blob SHAs to prevent multi-step rename leaks.
        let mut withheld: HashMap<String, String> = HashMap::new();
        let mut private_blobs: HashSet<String> = HashSet::new();
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
                    withheld.insert(
                        id.clone(),
                        format!("private path match: {}", hits.join(", ")),
                    );
                }
                for (path, blob_sha) in self.collect_tree_blobs(&record.tree, "")? {
                    if pol.path_is_private(&path) && blob_sha != EMPTY_BLOB {
                        private_blobs.insert(blob_sha);
                    }
                }
            }
            for id in self.index()? {
                if withheld.contains_key(&id) {
                    let record = self.get_change(&id)?;
                    for (_path, blob_sha) in self.collect_tree_blobs(&record.tree, "")? {
                        let existed_in_clean_parents = record.parents.iter().any(|p| {
                            !withheld.contains_key(p)
                                && self
                                    .get_change(p)
                                    .ok()
                                    .and_then(|pr| self.collect_tree_blobs(&pr.tree, "").ok())
                                    .is_some_and(|blobs| blobs.iter().any(|(_, b)| b == &blob_sha))
                        });
                        if !existed_in_clean_parents && blob_sha != EMPTY_BLOB {
                            private_blobs.insert(blob_sha);
                        }
                    }
                }
            }
        }

        let mut sha_of: HashMap<String, String> = HashMap::new();
        let mut source_sha_of: HashMap<String, String> = HashMap::new();
        // Exported sha -> rebuilt tree sha (filtered mode only).
        let mut tree_of: HashMap<String, String> = HashMap::new();
        let mut exported = Vec::new();
        let mut kept = 0u64;
        let mut rebuilt = 0u64;
        let mut sigs_dropped = 0u64;
        let mut resigned = 0u64;

        for id in self.index()? {
            let record = self.get_change(&id)?;
            if let Some(sha) = self.exported_sha(&id)? {
                if filtering {
                    let tree =
                        self.strip_tree(&record.tree, policy.unwrap(), &private_blobs, "")?;
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
                self.strip_tree(&record.tree, policy.unwrap(), &private_blobs, "")?
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
                        kept += 1;
                    }
                    exported.push((id.clone(), orig.clone()));
                    continue;
                }
            }

            // Reconstruction path: rebuild commit with remapped parents.
            // A rebuild drops the old sig since it covered the old bytes.
            // Only re-sign when the original carried a sig: a key set means
            // replace old sigs, never add new ones to unsigned commits.
            let had_sig = if filtering {
                match &record.source_sha {
                    Some(orig) => self.commit_had_sig(orig)?,
                    None => false,
                }
            } else {
                false
            };
            let active_key: Option<&str> = match (&sign_key, had_sig) {
                (Some(k), true) => Some(k.as_str()),
                _ => None,
            };
            let mut cmd = self.commit_tree_cmd(&stripped_tree, &record, active_key);
            for p in &parent_shas {
                cmd.args(["-p", p]);
            }
            let sha =
                self.finish_commit(cmd, &record.message, &id)
                    .map_err(|e| match active_key {
                        Some(k) => {
                            anyhow::anyhow!("re-sign with key '{k}' failed for change {id}: {e:#}")
                        }
                        None => e,
                    })?;
            if filtering {
                tree_of.insert(sha.clone(), stripped_tree);
                match (active_key, had_sig) {
                    (Some(k), true) => {
                        resigned += 1;
                        self.log_sig_event(
                            "resigned",
                            &id,
                            record.source_sha.as_deref(),
                            &format!("rebuilt commit re-signed with key '{k}'"),
                        )?;
                    }
                    (None, true) => {
                        sigs_dropped += 1;
                        self.log_sig_event(
                            "sig-dropped",
                            &id,
                            record.source_sha.as_deref(),
                            "rebuilt commit ships unsigned, original sig covered old bytes",
                        )?;
                    }
                    _ => rebuilt += 1,
                }
            }
            sha_of.insert(id.clone(), sha.clone());
            exported.push((id.clone(), sha));
        }
        if filtering {
            println!(
                "export: {kept} kept, {rebuilt} rebuilt unsigned, {sigs_dropped} lost sigs, {resigned} re-signed"
            );
        }
        Ok(exported)
    }

    /// Make `repo` self-contained: fold every object reachable from its
    /// refs into its own object database, then drop the `alternates` link
    /// that borrowed them from the store.
    ///
    /// `git repack -a -d` includes objects reached through `alternates`, so
    /// one command plus deleting the file is enough. A bundle that keeps
    /// the link is a bundle whose history is unreadable off this machine.
    fn detach_alternates(&self, repo: &Path) -> Result<()> {
        run(Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["repack", "-a", "-d"]))?;
        // `repack` leaves a stale alternates file behind on some paths, and
        // an absolute path to the sender's store is meaningless elsewhere.
        let alt = repo.join(".git/objects/info/alternates");
        if alt.exists() {
            std::fs::remove_file(&alt)?;
        }
        // Prove the bundle stands alone: with the store unreachable, every
        // ref must still resolve to a real object.
        let heads = run_stdout(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["rev-list", "--all"]),
        )?;
        if heads.trim().is_empty() {
            bail!("bundle has no reachable history after detaching objects");
        }
        Ok(())
    }

    /// Build a maintainer-only embargo bundle: full history with nothing
    /// withheld, plus dockets, export log, and a MANIFEST naming who gets it.
    /// Oot writes the plain bundle here; sealing happens in
    /// `embargo_bundle_sealed` through the gpg binary (same shell-out
    /// model as git). Sending stays with the courier: Oot never sends.
    pub fn embargo_bundle(
        &self,
        out_dir: &Path,
        policy: &VisibilityPolicy,
    ) -> Result<Vec<(String, String)>> {
        if !policy.is_under_embargo() {
            bail!("no active embargo: bundle needs embargo_until in the future");
        }
        let recipients: Vec<String> = policy
            .embargo_recipients
            .iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        if recipients.is_empty() {
            bail!("embargo bundle refused: embargo_recipients is empty");
        }
        refuse_symlink(out_dir, "bundle directory path")?;
        if out_dir.exists() {
            bail!("bundle directory already exists: {}", out_dir.display());
        }
        std::fs::create_dir_all(out_dir)?;
        // Plaintext private history goes inside; keep the bundle private on
        // disk until the user seals it, regardless of the umask.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(out_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        // Unfiltered replay: full history, private changes included.
        let repo_dir = out_dir.join("repo");
        std::fs::create_dir_all(&repo_dir)?;
        run(Command::new("git").args(["init", "--quiet"]).arg(&repo_dir))?;
        let exported = self.replay(&repo_dir, None)?;
        let tag_sign_key = policy
            .resign_key_id
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty());
        let pointed = self.point_branches_and_tags(&repo_dir, |_| true, tag_sign_key)?;
        if let Some((first, _)) = pointed.branches.first() {
            self.point_head(&repo_dir, first)?;
        }
        // The bundle leaves this machine, so it must not borrow objects:
        // `replay` attaches the store's odb through `alternates`, an
        // absolute path that means nothing on a maintainer's box. Repack
        // folds everything reachable into this repo and the link is cut,
        // so `git log` works on the far side with the store gone.
        self.detach_alternates(&repo_dir)?;
        // Sidecars travel with the bundle so maintainers can audit it.
        // Copy after logging: the bundle copy must hold this run's events.
        // Commit text can name private paths while blobs stay clean.
        // Taint only watches blobs, so warn and log instead of scrubbing.
        // Scrubbing would rewrite every hash and kill sigs.
        for (id, _) in &exported {
            let record = self.get_change(id)?;
            if message_names_private_path(&record.message, policy) {
                self.log_message_leak_warn(id, record.source_sha.as_deref())?;
                eprintln!("warning: change {id} message names a private path");
            }
        }
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": "embargo-bundle",
            "recipients": recipients,
            "changes": exported.len(),
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(EXPORT_LOG))?;
        writeln!(f, "{entry}")?;
        let dockets_src = self.root.join(crate::court::DOCKETS_DIR);
        if dockets_src.exists() {
            copy_dir(&dockets_src, &out_dir.join(crate::court::DOCKETS_DIR))?;
        }
        for log in [EXPORT_LOG, crate::court::ADJUDICATIONS_LOG] {
            let src = self.root.join(log);
            if src.exists() {
                std::fs::copy(&src, out_dir.join(log))?;
            }
        }
        let manifest = serde_json::json!({
            "embargo_until": policy.embargo_until,
            "recipients": recipients,
            "changes": exported
                .iter()
                .map(|(id, sha)| serde_json::json!({"change": id, "sha": sha}))
                .collect::<Vec<_>>(),
        });
        std::fs::write(
            out_dir.join("MANIFEST.json"),
            serde_json::to_string_pretty(&manifest)?,
        )?;
        Ok(exported)
    }

    /// Build the embargo bundle and seal it: the plaintext staging dir is
    /// tarred, then sign+encrypted to the recipients with gpg. Oot shells
    /// out to gpg the same way it already shells out to git for resigning;
    /// the math stays in someone else's binary. The staging dir and the
    /// intermediate plaintext tar are deleted on every path, success or not:
    /// plaintext never outlives the command. Oot still never sends — the
    /// sealed artifact is the courier's cargo.
    pub fn embargo_bundle_sealed(
        &self,
        out: &Path,
        policy: &VisibilityPolicy,
        signer_override: Option<&str>,
    ) -> Result<Vec<(String, String)>> {
        if !policy.is_under_embargo() {
            bail!("no active embargo: bundle needs embargo_until in the future");
        }
        refuse_symlink(out, "bundle artifact path")?;
        if out.exists() {
            bail!("bundle artifact already exists: {}", out.display());
        }
        let signer = signer_override
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| policy.resign_key_id.as_deref().map(str::trim).map(str::to_string))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "embargo seal needs a signer: set resign_key_id in visibility.toml or pass --signer"
                )
            })?;
        let recipients: Vec<String> = policy
            .embargo_recipients
            .iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        if recipients.is_empty() {
            bail!("embargo bundle refused: embargo_recipients is empty");
        }
        // Resolve every recipient against the local keyring before any
        // plaintext exists. Oot never fetches keys; the operator imports
        // them out of band.
        let mut refused = Vec::new();
        for r in &recipients {
            match gpg_key_state(r)? {
                GpgKeyState::Found => {}
                GpgKeyState::Unusable(why) => {
                    refused.push(format!("{r} ({why} key)"));
                }
                GpgKeyState::Missing => refused.push(format!("{r} (no key in keyring)")),
            }
        }
        // The signer needs a usable secret key too: discovering its absence
        // at `gpg --sign` time would waste a full plaintext build first.
        match gpg_secret_key_state(&signer)? {
            GpgSecretState::Usable => {}
            GpgSecretState::Unusable(why) => {
                refused.push(format!("{signer} ({why} signing key)"));
            }
            GpgSecretState::Missing => {
                refused.push(format!("{signer} (no signing key in keyring)"));
            }
        }
        if !refused.is_empty() {
            self.log_seal_event("seal-refused", serde_json::json!({ "unresolved": refused }))?;
            bail!(
                "embargo seal refused: unusable key for {} (import with `gpg --import`, refresh expired keys)",
                refused.join(", ")
            );
        }
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let staging = sealed_staging_dir(out);
        let mut tar_name = staging.as_os_str().to_os_string();
        tar_name.push(".tar");
        let tar_path = std::path::PathBuf::from(tar_name);
        // `--out x.tar` derives staging `x` and tar `x.tar`, which IS the
        // artifact. gpg happens to fail on that today; refuse it outright
        // rather than depend on someone else's binary being careful.
        if tar_path == out {
            bail!(
                "refusing to seal: --out {} derives its own tar name; name the artifact *.tar.gpg",
                out.display()
            );
        }
        // The staging dir holds the full unfiltered history and the tar is
        // plaintext too: the guard removes both on every exit path — return,
        // error, or panic — so plaintext never outlives the command. Arming
        // proves both paths are absent first, so a refusal never deletes
        // data the user already had at the predicted staging/tar names.
        let guard = PlaintextGuard::arm(vec![staging.clone(), tar_path.clone()])?;
        let result = self.build_and_seal(&staging, &tar_path, out, policy, &signer, &recipients);
        let exported = result.inspect_err(|e| {
            // The artifact did not exist on entry: a partial file from a
            // failed seal must not linger where an operator could ship it.
            let _ = std::fs::remove_file(out);
            let _ =
                self.log_seal_event("seal-failed", serde_json::json!({ "error": e.to_string() }));
        })?;
        self.log_seal_event(
            "embargo-sealed",
            serde_json::json!({
                "signer": signer,
                "recipients": recipients,
                "changes": exported.len(),
                "artifact": out.display().to_string(),
            }),
        )
        .inspect_err(|_| {
            // Fail closed: a sealed artifact with no audit event must not
            // ship, so remove it when the log write fails.
            let _ = std::fs::remove_file(out);
        })?;
        // Explicit cleanup, not just Drop: plaintext that outlives the
        // command must fail the command, not print `sealed` and exit 0.
        // Drop still runs afterwards and is a no-op once this succeeds.
        let cleanup = guard.remove_now();
        if cleanup.is_err() {
            // Never hand back an artifact whose plaintext we could not
            // remove: that is the one state an operator must never ship.
            let _ = std::fs::remove_file(out);
        }
        cleanup?;
        Ok(exported)
    }

    fn build_and_seal(
        &self,
        staging: &Path,
        tar_path: &Path,
        out: &Path,
        policy: &VisibilityPolicy,
        signer: &str,
        recipients: &[String],
    ) -> Result<Vec<(String, String)>> {
        let exported = self.embargo_bundle(staging, policy)?;
        run(Command::new("tar")
            .args(["-cf"])
            .arg(tar_path)
            .arg("-C")
            // A bare relative `--out` like `embargo.tar.gpg` has an empty
            // parent, and `tar -C ""` fails outright.
            .arg(
                staging
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new(".")),
            )
            .arg("--")
            .arg(staging.file_name().unwrap_or_default()))?;
        // The tar is plaintext: match the staging dir's 0700 stance on the
        // file itself so the window before encryption is local-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tar_path, std::fs::Permissions::from_mode(0o600))?;
        }
        let mut gpg = Command::new("gpg");
        gpg.args([
            "--batch",
            "--yes",
            "--trust-model",
            "always",
            "--encrypt",
            "--sign",
        ]);
        gpg.args(["--local-user", signer]);
        for r in recipients {
            gpg.args(["--recipient", r]);
        }
        gpg.arg("--output").arg(out).arg("--").arg(tar_path);
        run(&mut gpg)?;
        Ok(exported)
    }

    /// Record a sealing decision in the export audit log: refusal, failure,
    /// or the sealed artifact itself.
    fn log_seal_event(&self, event: &str, extra: serde_json::Value) -> Result<()> {
        let mut entry = serde_json::json!({ "epoch": now_epoch(), "event": event });
        if let (Some(obj), Some(extras)) = (entry.as_object_mut(), extra.as_object()) {
            for (k, v) in extras {
                obj.insert(k.clone(), v.clone());
            }
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(EXPORT_LOG))?;
        writeln!(f, "{entry}")?;
        Ok(())
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
    /// fragments or tainted private blob SHAs, recursively. Pure plumbing
    /// (`ls-tree` + `mktree`) against the store's bare odb — no index or
    /// worktree involved. Deterministic: identical inputs yield the original sha untouched.
    fn strip_tree(
        &self,
        tree: &str,
        policy: &VisibilityPolicy,
        private_blobs: &HashSet<String>,
        prefix: &str,
    ) -> Result<String> {
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
                    if policy.path_is_private(&path) || private_blobs.contains(&sha) {
                        changed = true;
                        continue;
                    }
                    lines.push(record.to_string());
                }
                "tree" => {
                    let sub = self.strip_tree(&sha, policy, private_blobs, &format!("{path}/"))?;
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
    fn commit_tree_cmd(
        &self,
        tree: &str,
        record: &ChangeRecord,
        sign_key: Option<&str>,
    ) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(["--git-dir"])
            .arg(self.git_dir())
            .arg("commit-tree")
            .arg(tree);
        if let Some(key) = sign_key {
            cmd.arg(format!("-S{key}"));
        }
        cmd.env(
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

    /// Record a rebuilt commit's signature fate in `.oot/export-log.jsonl`:
    /// either the old sig was dropped or a maintainer key re-signed it.
    fn log_sig_event(
        &self,
        event: &str,
        id: &str,
        source_sha: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": event,
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

    fn log_message_leak_warn(&self, id: &str, source_sha: Option<&str>) -> Result<()> {
        let entry = serde_json::json!({
            "epoch": now_epoch(),
            "event": "message-leak-warn",
            "change": id,
            "source_sha": source_sha,
            "reason": "commit message names a private path, blobs are clean",
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

    /// Point branches and tags at their exported shas in a freshly replayed
    /// repo. `keep_branch` filters whole branches: public export drops private
    /// branches, the embargo bundle keeps everything. Omissions land in the
    /// audit log and on stderr and never abort the run — one bad tag must not
    /// sink a whole history replay. Returns what was pointed, for reporting
    /// and HEAD selection.
    pub fn point_branches_and_tags(
        &self,
        repo_dir: &Path,
        keep_branch: impl Fn(&str) -> bool,
        resign_key: Option<&str>,
    ) -> Result<PointedRefs> {
        let mut pointed = PointedRefs::default();
        for (branch, head_id) in self.refs()? {
            if !keep_branch(&branch) {
                self.log_branch_omitted(&branch, &head_id)?;
                continue;
            }
            match self.branch_head_sha(&head_id)? {
                Some(sha) => {
                    self.point_ref(repo_dir, &branch, &sha)?;
                    pointed.branches.push((branch, sha));
                }
                None => {
                    self.log_branch_omitted(&branch, &head_id)?;
                    eprintln!("warning: branch {branch} omitted (entire history withheld)");
                }
            }
        }
        for (tag, head_id) in self.tags()? {
            if !Self::valid_tag_ref(&tag) {
                self.log_tag_omitted(&tag, &head_id)?;
                eprintln!("warning: tag {tag} omitted (invalid refname)");
                continue;
            }
            match self.branch_head_sha(&head_id) {
                Ok(Some(sha)) => {
                    // Prefer the original annotated tag object: when the
                    // tagged commit exported byte-identically, the object
                    // (and its signature) stays valid verbatim.
                    match self.source_tag_object(&tag)? {
                        Some((obj, target)) if target == sha => {
                            match self.point_tag_object(repo_dir, &tag, &obj) {
                                Ok(()) => pointed.tags.push((tag, sha)),
                                Err(e) => {
                                    self.log_tag_omitted(&tag, &head_id)?;
                                    eprintln!("warning: tag {tag} omitted ({e:#})");
                                }
                            }
                        }
                        source => {
                            // Rebuilt or lightweight target: the original
                            // object cannot ride along, but the tag's
                            // identity — tagger and message — lives in the
                            // store from import, so recreate the annotated
                            // tag at the exported target. With a key it
                            // ships freshly signed; without one it is
                            // unsigned and the signature loss is audited.
                            // A signing failure is a config error and
                            // fails the export loudly, like a bad
                            // resign_key_id on a rebuilt commit.
                            let own_target = self
                                .get_change(&head_id)
                                .ok()
                                .and_then(|r| r.source_sha.clone());
                            let originally_signed = match &source {
                                Some((obj, peel))
                                    if own_target.as_deref() == Some(peel.as_str()) =>
                                {
                                    self.tag_object_signed(obj)?
                                }
                                _ => self.tag_meta(&tag)?.is_some_and(|m| m.signed),
                            };
                            let meta = self.tag_meta(&tag)?;
                            let recreatable = meta.as_ref().and_then(|m| {
                                match (&m.tagger_name, &m.tagger_email) {
                                    (Some(n), Some(e)) if !n.is_empty() && !e.is_empty() => Some(m),
                                    _ => None,
                                }
                            });
                            match recreatable {
                                Some(meta) => {
                                    self.recreate_tag(repo_dir, &tag, &sha, meta, resign_key)?;
                                    if resign_key.is_some() {
                                        self.log_tag_resigned(
                                            &tag,
                                            &head_id,
                                            resign_key.unwrap_or_default(),
                                        )?;
                                    } else if originally_signed {
                                        self.log_tag_sig_dropped(&tag, &head_id)?;
                                    }
                                    pointed.tags.push((tag, sha));
                                }
                                None => {
                                    // No stored identity (old stores,
                                    // lightweight source tags): a
                                    // lightweight ref is all that survives.
                                    if originally_signed {
                                        self.log_tag_sig_dropped(&tag, &head_id)?;
                                    }
                                    match self.point_tag(repo_dir, &tag, &sha) {
                                        Ok(()) => pointed.tags.push((tag, sha)),
                                        Err(e) => {
                                            self.log_tag_omitted(&tag, &head_id)?;
                                            eprintln!("warning: tag {tag} omitted ({e:#})");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(None) => {
                    self.log_tag_omitted(&tag, &head_id)?;
                    eprintln!("warning: tag {tag} omitted (entire history withheld)");
                }
                Err(e) => {
                    self.log_tag_omitted(&tag, &head_id)?;
                    eprintln!("warning: tag {tag} omitted ({e:#})");
                }
            }
        }
        Ok(pointed)
    }

    /// Point HEAD at the first exported branch and populate the working tree
    /// so the exported repo opens ready to inspect. Checkout is best-effort:
    /// refs and objects are the deliverable, a failed checkout only leaves
    /// the tree empty, so warn and keep going.
    pub fn point_head(&self, repo_dir: &Path, first_branch: &str) -> Result<()> {
        let sym = format!("refs/heads/{first_branch}");
        run(Command::new("git")
            .args(["symbolic-ref", "HEAD", &sym])
            .current_dir(repo_dir))?;
        match Command::new("git")
            .args(["checkout", "-f", "HEAD"])
            .current_dir(repo_dir)
            .output()
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => eprintln!(
                "warning: could not populate the exported working tree: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => eprintln!("warning: could not populate the exported working tree: {e}"),
        }
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

    /// Whether the original commit object carries a gpgsig header.
    /// Covers both `gpgsig` and `gpgsig-sha256`. Header lines only: stop at
    /// the first blank line so a message starting with `gpgsig ` never fakes
    /// a signature. Import never records this, so ask the object database
    /// directly. Fails loudly when the object cannot be read: silent false
    /// means a dropped sig goes unlogged.
    fn commit_had_sig(&self, sha: &str) -> Result<bool> {
        let out = Command::new("git")
            .args(["--git-dir"])
            .arg(self.git_dir())
            .args(["cat-file", "commit", sha])
            .output()
            .context("failed to read commit object")?;
        if !out.status.success() {
            anyhow::bail!(
                "failed to read commit object {sha}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .take_while(|l| !l.is_empty())
            .any(|l| l.starts_with("gpgsig ") || l.starts_with("gpgsig-sha256 ")))
    }

    /// The exported commit sha for a change id, if this store has exported before.
    pub fn exported_sha(&self, id: &str) -> Result<Option<String>> {
        let f = self.export_map_path(id);
        if !f.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(f)?.trim().to_string()))
    }

    /// Discover all change IDs reachable from the given roots and all
    /// branch and tag refs. Fails loudly if any reachable change record
    /// cannot be read.
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
        for (_, head_id) in self.tags()? {
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

/// Refs a replay pointed in an exported repo, for reporting and HEAD selection.
#[derive(Debug, Default)]
pub struct PointedRefs {
    pub branches: Vec<(String, String)>,
    pub tags: Vec<(String, String)>,
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

/// Run a command and hand back its stdout, for callers that read a value
/// rather than just checking the exit status.
fn run_stdout(cmd: &mut Command) -> Result<String> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {}", cmd.get_program().to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "{} failed: {}",
            cmd.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run(cmd: &mut Command) -> Result<()> {
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {}", cmd.get_program().to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "{} failed: {}",
            cmd.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// A tag's original identity, captured from the source repo at import.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TagMeta {
    pub tagger_name: Option<String>,
    pub tagger_email: Option<String>,
    pub message: String,
    pub signed: bool,
}

/// Whether a message part carries a known signature armor header.
fn message_has_signature(message: &str) -> bool {
    message.contains("-----BEGIN PGP SIGNATURE")
        || message.contains("-----BEGIN SSH SIGNATURE")
        || message.contains("-----BEGIN SIGNED MESSAGE")
}

/// Drop a trailing signature block from a tag message, keeping the prose.
fn strip_signature_block(message: &str) -> String {
    for start in [
        "-----BEGIN PGP SIGNATURE-----",
        "-----BEGIN SSH SIGNATURE-----",
        "-----BEGIN SIGNED MESSAGE-----",
    ] {
        if let Some(i) = message.find(start) {
            return message[..i].trim_end().to_string();
        }
    }
    message.to_string()
}

/// Whether `entry` (email or fingerprint) resolves to a key in the local
/// gpg keyring. Oot only resolves; it never fetches keys. Unusable keys
/// (revoked, expired, disabled) refuse here so the seal never gets as far
/// as building plaintext; unknown trust is fine because the encrypt step
/// runs with `--trust-model always` — the recipients named in the policy
/// are the authorization.
enum GpgKeyState {
    Found,
    Unusable(&'static str),
    Missing,
}

fn gpg_key_state(entry: &str) -> Result<GpgKeyState> {
    let out = Command::new("gpg")
        .args(["--list-keys", "--with-colons", "--", entry])
        .output()
        .map_err(|e| {
            anyhow::anyhow!("failed to run gpg ({e}); install gnupg to seal embargo bundles")
        })?;
    if !out.status.success() {
        return Ok(GpgKeyState::Missing);
    }
    let mut state = GpgKeyState::Missing;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.first() == Some(&"pub") && fields.len() > 1 {
            state = match fields[1] {
                "r" => GpgKeyState::Unusable("revoked"),
                "e" => GpgKeyState::Unusable("expired"),
                "d" => GpgKeyState::Unusable("disabled"),
                _ => GpgKeyState::Found,
            };
            if matches!(state, GpgKeyState::Found) {
                break;
            }
        }
    }
    Ok(state)
}

/// Removes plaintext artifacts (the staging dir and the intermediate tar)
/// on every exit path — return, error, or panic — so the unfiltered history
/// never outlives the command.
///
/// Armed only with paths Oot itself created. The caller must prove each
/// path was absent (refusing symlinks and pre-existing files/dirs) before
/// arming, because `Drop` cannot ask: a guard pointed at a path the user
/// already had would delete their data on a clean refusal. `Drop` also
/// re-checks with `symlink_metadata` so a swapped-in symlink is unlinked
/// rather than followed.
#[derive(Debug)]
struct PlaintextGuard {
    paths: Vec<std::path::PathBuf>,
}

impl PlaintextGuard {
    /// Arm a guard only if every path is absent, symlinks included. A path
    /// that already exists is reported and left untouched, so the command
    /// refuses instead of destroying whatever lives there.
    fn arm(paths: Vec<std::path::PathBuf>) -> Result<Self> {
        for path in &paths {
            if let Ok(meta) = std::fs::symlink_metadata(path) {
                let kind = if meta.file_type().is_symlink() {
                    "symlink"
                } else if meta.is_dir() {
                    "directory"
                } else {
                    "file"
                };
                bail!(
                    "refusing to seal: plaintext path already exists ({kind}): {}",
                    path.display()
                );
            }
        }
        Ok(PlaintextGuard { paths })
    }
}

impl PlaintextGuard {
    /// Delete the guarded plaintext now, reporting any failure. `Drop`
    /// cannot propagate an error, so the seal path calls this explicitly:
    /// reporting `sealed` while plaintext survives is a lie, so a failed
    /// cleanup must fail the command and delete the artifact too.
    fn remove_now(&self) -> Result<()> {
        for path in &self.paths {
            // `symlink_metadata`, not `is_dir`: a link swapped in after
            // arming must be unlinked, never traversed with remove_dir_all.
            let is_dir = std::fs::symlink_metadata(path)
                .map(|m| !m.file_type().is_symlink() && m.is_dir())
                .unwrap_or(false);
            let result = if is_dir {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            match result {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => bail!(
                    "seal cleanup failed: could not remove plaintext {}: {e}",
                    path.display()
                ),
            }
        }
        Ok(())
    }
}

impl Drop for PlaintextGuard {
    fn drop(&mut self) {
        // `remove_now` already reported; here only best-effort so a normal
        // early return never leaves plaintext behind.
        let _ = self.remove_now();
    }
}

/// The staging dir for a sealed artifact: the artifact path with its
/// extensions stripped, so extracting the tarball lands in a directory
/// named like the bundle. `embargo-2099.tar.gpg` -> `embargo-2099/`.
pub fn sealed_staging_dir(out: &Path) -> std::path::PathBuf {
    let name = out
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("embargo-bundle")
        .trim_end_matches(".tar")
        .to_string();
    out.parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
        .join(name)
}

/// What the recipient learns from opening a sealed embargo bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBundle {
    /// Signing key fingerprint from gpg VALIDSIG.
    pub signer: String,
    /// Recipients named in MANIFEST.json.
    pub recipients: Vec<String>,
    /// Embargo date from MANIFEST.json, e.g. `2099-01-01`.
    pub embargo_until: String,
    /// Number of changes listed in MANIFEST.json.
    pub changes: usize,
}

/// Open a sealed embargo bundle on the recipient side: decrypt with gpg
/// (which also verifies the signature), unpack the tarball, and report
/// the MANIFEST. Oot still never sends — the operator moves the artifact
/// out of band over an existing secure channel; this only opens what
/// arrived.
///
/// Both `VALIDSIG` and `DECRYPTION_OKAY` are required in gpg's status
/// output, and any `BADSIG`/`ERRSIG`/expiry/revocation marker refuses the
/// open — even beside a `VALIDSIG`. That expiry strictness is deliberate:
/// old bundles stop opening when the signer key dies, so re-seal under a
/// live key instead of overriding the check. With `expect_signer`, the VALIDSIG fingerprint must match a full
/// fingerprint or a trailing key-id suffix (case-insensitive, at least 16
/// hex chars). No network, no key fetch: both sides import keys out of
/// band first. The intermediate plaintext tar is deleted; the extracted
/// tree under `out_dir` is chmod 0700.
pub fn embargo_verify(
    artifact: &Path,
    out_dir: &Path,
    expect_signer: Option<&str>,
) -> Result<VerifiedBundle> {
    if !artifact.is_file() {
        if artifact.exists() {
            bail!("not a bundle file: {}", artifact.display());
        }
        bail!("no such bundle artifact: {}", artifact.display());
    }
    refuse_symlink(out_dir, "verify output path")?;
    if out_dir.exists() {
        bail!("output already exists: {}", out_dir.display());
    }
    if let Some(expected) = expect_signer {
        let norm: String = expected.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if norm.len() < 16 {
            bail!("--expect-signer needs at least 16 hex chars");
        }
    }
    if let Some(parent) = out_dir.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::create_dir_all(out_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp_tar = {
        let name = out_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("embargo-bundle");
        out_dir
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("{name}.decrypting.tar"))
    };
    if tmp_tar.exists() {
        std::fs::remove_dir_all(out_dir)?;
        bail!(
            "temporary tar already exists (stale verify?): {}",
            tmp_tar.display()
        );
    }
    // The temp tar is written by gpg with `--yes`: a planted symlink here
    // would redirect decrypted plaintext, so refuse links, not just files.
    // Like every other refusal here, this must not leave the output tree
    // behind, or the retry hits a misleading "already exists".
    if let Err(e) = refuse_symlink(&tmp_tar, "verify temporary tar path") {
        let _ = std::fs::remove_dir_all(out_dir);
        return Err(e);
    }
    let cleanup = || {
        let _ = std::fs::remove_file(&tmp_tar);
    };
    let decrypt = Command::new("gpg")
        .args([
            "--batch",
            "--yes",
            "--status-fd",
            "1",
            "--decrypt",
            "--output",
        ])
        .arg(&tmp_tar)
        .arg("--")
        .arg(artifact)
        .output()
        .map_err(|e| anyhow!("failed to run gpg ({e}); install gnupg to open embargo bundles"))?;
    let status = String::from_utf8_lossy(&decrypt.stdout).to_string();
    if !decrypt.status.success() {
        cleanup();
        let _ = std::fs::remove_dir_all(out_dir);
        bail!(
            "gpg decrypt failed: {}",
            String::from_utf8_lossy(&decrypt.stderr).trim()
        );
    }
    // VALIDSIG field 1 is the *signing subkey*; the final field is the
    // primary fingerprint. Keys that sign through a subkey (the normal
    // case for modern RSA setups) must still match the primary fingerprint
    // the operator reads off `gpg --fingerprint` and writes into
    // `visibility.toml`, so both are kept and either satisfies a pin.
    let status = parse_gpg_status(&status);
    if let Some(bad) = &status.bad_marker {
        cleanup();
        let _ = std::fs::remove_dir_all(out_dir);
        bail!("gpg reported a failed signature status (refusing to open): {bad}");
    }
    let Some(signing_key) = status.signing_key.clone() else {
        cleanup();
        let _ = std::fs::remove_dir_all(out_dir);
        bail!("gpg reported no valid signature (refusing to open)");
    };
    if !status.decrypted_ok {
        cleanup();
        let _ = std::fs::remove_dir_all(out_dir);
        bail!("gpg reported no DECRYPTION_OKAY (refusing to open)");
    }
    if let Some(expected) = expect_signer {
        let norm = |s: &str| {
            s.chars()
                .filter(|c| c.is_ascii_hexdigit())
                .collect::<String>()
                .to_uppercase()
        };
        let want = norm(expected);
        // Either the signing subkey or the primary fingerprint satisfies
        // the pin: an operator's `gpg --fingerprint` output names the
        // primary, and that is what visibility.toml records.
        let candidates = [
            Some(norm(&signing_key)),
            status.primary_key.as_deref().map(norm),
        ];
        let matched = candidates.iter().flatten().any(|got| got.ends_with(&want));
        if !matched {
            cleanup();
            let _ = std::fs::remove_dir_all(out_dir);
            bail!("signer mismatch: bundle signed by {signing_key}, expected {expected}");
        }
    }
    // Report the primary fingerprint when gpg gave one: that is the stable
    // identity an operator records, and it is what a subkey pin resolves to.
    let signer = status.primary_key.unwrap_or(signing_key);
    if let Err(e) = (|| -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_tar, std::fs::Permissions::from_mode(0o600))?;
        }
        // Bounds and member policy are checked before a single byte is
        // unpacked, and extraction refuses to overwrite: hostile tars stay
        // outside and cannot exhaust the recipient's disk.
        check_tar_members(&tmp_tar)?;
        run(Command::new("tar")
            .arg("--extract")
            .arg("--keep-old-files")
            .arg("--file")
            .arg(&tmp_tar)
            .arg("--directory")
            .arg(out_dir))?;
        Ok(())
    })() {
        cleanup();
        let _ = std::fs::remove_dir_all(out_dir);
        return Err(e);
    }
    if let Err(e) = std::fs::remove_file(&tmp_tar) {
        if e.kind() != std::io::ErrorKind::NotFound {
            let _ = std::fs::remove_dir_all(out_dir);
            return Err(anyhow!(
                "could not remove decrypted plaintext {}: {e}",
                tmp_tar.display()
            ));
        }
    }
    // Manifest discovery follows the seal layout (bundle root for --plain
    // style, one level down for sealed artifacts). Every candidate is
    // symlink-checked first: `is_file` follows links, and a crafted tar
    // could otherwise point the manifest — or the repo — outside.
    let manifest_path = {
        let direct = out_dir.join("MANIFEST.json");
        if is_plain_file(&direct) {
            direct
        } else {
            let mut found: Option<PathBuf> = None;
            for entry in std::fs::read_dir(out_dir)? {
                let entry = entry?;
                if !is_plain_dir(&entry.path()) {
                    continue;
                }
                let candidate = entry.path().join("MANIFEST.json");
                if is_plain_file(&candidate) {
                    found = Some(candidate);
                    break;
                }
            }
            found.ok_or_else(|| {
                anyhow!(
                    "decrypted bundle has no MANIFEST.json under {}",
                    out_dir.display()
                )
            })?
        }
    };
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?,
    )
    .context("bundle MANIFEST.json is not valid JSON")?;
    let embargo_until = manifest
        .get("embargo_until")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let recipients = manifest
        .get("recipients")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let changes = manifest
        .get("changes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Ok(VerifiedBundle {
        signer,
        recipients,
        embargo_until,
        changes,
    })
}

/// Refuse paths that are symlinks: seal/verify outputs must be plain
/// files or dirs created by Oot, never planted links that would redirect
/// plaintext, signatures, or decrypted trees elsewhere. Uses
/// `symlink_metadata` so dangling links are caught too (`exists` follows
/// links and would miss them).
fn refuse_symlink(path: &Path, what: &str) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            bail!("{} must not be a symlink: {}", what, path.display());
        }
    }
    Ok(())
}

/// The secret-key state for `entry` in the local keyring: absent, present
/// but unusable, or usable. Oot only resolves; it never generates or
/// fetches keys.
enum GpgSecretState {
    Usable,
    Unusable(&'static str),
    Missing,
}

fn gpg_secret_key_state(entry: &str) -> Result<GpgSecretState> {
    let out = Command::new("gpg")
        .args(["--list-secret-keys", "--with-colons", "--", entry])
        .output()
        .map_err(|e| {
            anyhow::anyhow!("failed to run gpg ({e}); install gnupg to seal embargo bundles")
        })?;
    if !out.status.success() {
        return Ok(GpgSecretState::Missing);
    }
    // Field 2 of the `sec` record carries validity (`e`/`r`/`d`). An
    // unusable key must not be reported as "no key in keyring": the
    // operator would be told to import a key they already hold.
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if !line.starts_with("sec:") {
            continue;
        }
        return Ok(match line.split(':').nth(1) {
            Some("e") => GpgSecretState::Unusable("expired"),
            Some("r") => GpgSecretState::Unusable("revoked"),
            Some("d") => GpgSecretState::Unusable("disabled"),
            _ => GpgSecretState::Usable,
        });
    }
    Ok(GpgSecretState::Missing)
}

/// True for a regular file that is not a symlink. `Path::is_file` follows
/// links, so extracted-bundle lookups must use this instead.
fn is_plain_file(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path).map(|m| (m.file_type().is_symlink(), m.is_file())),
        Ok((false, true))
    )
}

/// True for a real directory that is not a symlink.
fn is_plain_dir(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path).map(|m| (m.file_type().is_symlink(), m.is_dir())),
        Ok((false, true))
    )
}

/// Caps on an incoming bundle. A signed artifact is still attacker-
/// reachable when a signer key leaks, so extraction is bounded: a ~3 MB
/// hostile tar must not be able to exhaust a recipient's disk. 4 GiB of
/// plain files is far beyond any real code bundle, and the byte cap is
/// deliberately under the 8 GiB ceiling of a ustar octal size field so it
/// is reachable by a crafted header.
const MAX_BUNDLE_MEMBERS: usize = 100_000;
const MAX_BUNDLE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Whether `s` is a plausible key id: hex only, at least 8 chars. Keeps a
/// malformed gpg status field from being used as a fingerprint.
/// What gpg's `--status-fd` said about an opened bundle.
#[derive(Debug, Default, PartialEq, Eq)]
struct GpgStatus {
    /// VALIDSIG field 1: the key that actually made the signature, which is
    /// a signing subkey for most modern keys.
    signing_key: Option<String>,
    /// The final VALIDSIG field: the primary key fingerprint.
    primary_key: Option<String>,
    /// Whether decryption completed.
    decrypted_ok: bool,
    /// A bad/expired/revoked signature marker, if gpg emitted one. Any of
    /// these poisons the open even beside a VALIDSIG line: a key that died
    /// after sealing must stop opening bundles.
    bad_marker: Option<String>,
}

/// Parse gpg's status output. Pure, so the rules are unit-testable without
/// a live keyring: an expired or revoked signer is awkward to stage on
/// demand (gpg has no scriptable revoke), but its status line is just text.
fn parse_gpg_status(raw: &str) -> GpgStatus {
    let mut out = GpgStatus::default();
    for line in raw.lines() {
        let line = line.strip_prefix("[GNUPG:] ").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("VALIDSIG ") {
            let mut fields = rest.split_whitespace();
            out.signing_key = fields.next().map(str::to_string);
            // The primary key fingerprint is the final field. After
            // `VALIDSIG <subkey>` the line is
            // `date ts expire version reserved pubkeyalgo hashalgo sigclass
            // <primary-fpr>`, i.e. token index 9 counting the subkey as 0.
            if let Some(primary) = fields.nth(8) {
                if is_hex_keyid(primary) {
                    out.primary_key = Some(primary.to_string());
                }
            }
        } else if line.starts_with("DECRYPTION_OKAY") {
            out.decrypted_ok = true;
        } else if line.starts_with("BADSIG")
            || line.starts_with("ERRSIG")
            || line.starts_with("EXPSIG")
            || line.starts_with("EXPKEYSIG")
            || line.starts_with("REVKEYSIG")
            || line.starts_with("KEYREVOKED")
        {
            out.bad_marker = Some(line.to_string());
        }
    }
    out
}

fn is_hex_keyid(s: &str) -> bool {
    s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Reject a bundle tar before extraction: unsafe member names (absolute or
/// `..`), member counts or sizes past the caps, and member types an
/// embargo bundle never contains. Oot-made bundles hold exactly one top
/// dir with a git repo, dockets, logs, and MANIFEST.json — all regular
/// files, directories, and symlinks Oot never writes.
///
/// Names are only part of the defense: GNU tar itself refuses `..`
/// traversal, absolute members, and cross-device hard links. This check
/// fails early and explicitly so a hostile tar is never unpacked at all.
fn check_tar_members(tar_path: &Path) -> Result<()> {
    // Caps first, from a raw header walk. Sizes are attacker-controlled and
    // a sparse archive declares gigabytes it never stores, so GNU tar may
    // refuse to list it at all: the caps must hold regardless of whether
    // tar can parse the file.
    let (members, bytes) = tar_header_totals(tar_path)?;
    if members == 0 {
        bail!("bundle tar is empty (refusing to open)");
    }
    if members > MAX_BUNDLE_MEMBERS as u64 {
        bail!("bundle tar has more than {MAX_BUNDLE_MEMBERS} members (refusing to open)");
    }
    if bytes > MAX_BUNDLE_BYTES {
        bail!("bundle tar expands past {MAX_BUNDLE_BYTES} bytes (refusing to open)");
    }
    // Then names and types, via tar itself so long-name and pax encodings
    // are interpreted correctly instead of guessed at.
    let listing = Command::new("tar")
        .arg("--list")
        .arg("--verbose")
        .arg("--numeric-owner")
        .arg("--file")
        .arg(tar_path)
        .output()
        .map_err(|e| anyhow!("failed to list bundle tar ({e})"))?;
    if !listing.status.success() {
        bail!(
            "bundle tar is unreadable: {}",
            String::from_utf8_lossy(&listing.stderr).trim()
        );
    }
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let Some((meta, rest)) = line.split_once(' ') else {
            continue;
        };
        // `-rw-r--r-- root/root  1234 2026-09-26 00:00 bundle/repo`
        // `lrwxrwxrwx root/root     0 2026-09-26 00:00 bundle/link -> ../x`
        // The name is the last token only for files and directories: a
        // symlink line ends with its TARGET. Taking the last token checks
        // the target and never the name, so a bundle with an ordinary
        // `-> ../shared` link is refused while a hostile name sails past.
        // Split the arrow off first, then read the fixed columns:
        // owner, size, date, time, name.
        let tail = match rest.split_once(" -> ") {
            Some((head, _target)) => head,
            None => rest,
        };
        let Some(name) = tail.split_whitespace().nth(4) else {
            continue;
        };
        let name = name.trim_end_matches('/');
        if name.is_empty() {
            continue;
        }
        if name.starts_with('/') || name.split('/').any(|c| c == "..") {
            bail!("bundle tar has unsafe member (refusing to open): {name}");
        }
        match meta.chars().next().unwrap_or('-') {
            // Regular files, directories, and symlinks only. FIFOs, block
            // and character devices have no place in a code bundle and are
            // a way to make a recipient's tooling misbehave.
            '-' | 'd' | 'l' => {}
            other => {
                bail!("bundle tar has unsupported member type '{other}' (refusing to open): {name}")
            }
        }
    }
    Ok(())
}

/// Walk ustar/GNU headers to count members and total the declared payload
/// sizes. Reads headers only, so a sparse archive that declares gigabytes
/// costs nothing to reject. Returns (member count, declared bytes).
fn tar_header_totals(tar_path: &Path) -> Result<(u64, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let file = std::fs::File::open(tar_path)
        .with_context(|| format!("failed to open {}", tar_path.display()))?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut header = [0u8; 512];
    let mut members: u64 = 0;
    let mut bytes: u64 = 0;
    // Bounded so a header run of zeros or a malformed archive cannot spin.
    while members < 2_000_000 {
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            // Running out of bytes before the end-of-archive marker means
            // the archive is shorter than its own headers claim. Oot never
            // writes sparse tars, so treat it as corrupt or hostile rather
            // than quietly trusting the totals gathered so far.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                bail!("bundle tar is truncated (refusing to open)");
            }
            Err(e) => return Err(anyhow!("bundle tar read failed: {e}")),
        }
        if header.iter().all(|b| *b == 0) {
            break; // end-of-archive marker
        }
        members += 1;
        // Octal size at offset 124, 12 bytes, NUL/space terminated. GNU
        // base-256 (high bit set) means a value too large for octal fields:
        // saturate rather than trust it.
        let raw = &header[124..136];
        let size = if raw[0] & 0x80 != 0 {
            u64::MAX / 4
        } else {
            let text: String = raw
                .iter()
                .take_while(|b| **b != 0 && **b != b' ')
                .map(|b| *b as char)
                .collect();
            u64::from_str_radix(text.trim(), 8).unwrap_or(0)
        };
        // Typeflag '5' is a directory: it declares no payload.
        if header[156] != b'5' {
            bytes = bytes.saturating_add(size);
            // Check as we go, before touching the payload: a sparse member
            // declaring more than the whole cap must be refused without the
            // archive ever having to be that large on disk.
            if bytes > MAX_BUNDLE_BYTES {
                bail!("bundle tar expands past {MAX_BUNDLE_BYTES} bytes (refusing to open)");
            }
        }
        // Skip the payload, padded to a 512 boundary.
        let padded = size.div_ceil(512) * 512;
        if padded > 0 && reader.seek(SeekFrom::Current(padded as i64)).is_err() {
            // The archive is shorter than its own headers claim: a sparse
            // member, which Oot never writes. Report the byte cap when that
            // is the real objection, truncation otherwise.
            if bytes > MAX_BUNDLE_BYTES {
                bail!("bundle tar expands past {MAX_BUNDLE_BYTES} bytes (refusing to open)");
            }
            bail!("bundle tar is truncated (refusing to open)");
        }
    }
    Ok((members, bytes))
}

fn message_names_private_path(message: &str, policy: &VisibilityPolicy) -> bool {
    let lower = message.to_lowercase();
    policy
        .private_paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .any(|p| {
            let needle = p.trim().to_lowercase();
            // A mention needs a boundary before the path, so `nonsecrets/`
            // does not trip on `secrets/`. Suffix-like patterns (.pem, /etc)
            // may sit mid-token: `foo.pem` is a real mention.
            let suffix_like = needle.starts_with('.') || needle.starts_with('/');
            let mut from = 0;
            while let Some(pos) = lower[from..].find(&needle) {
                let at = from + pos;
                let boundary = at == 0
                    || suffix_like
                    || !lower[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
                if boundary {
                    return true;
                }
                from = at + needle.len();
            }
            false
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir for a guard test.
    fn guard_tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oot-guard-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Parse gpg's status output. These assertions are the only coverage of
    /// the "signer died after sealing" rule: gpg cannot be scripted to
    /// revoke a key in a test, but the status lines it would emit are just
    /// text, and a VALIDSIG sitting next to an expiry marker must still
    /// poison the open.
    #[test]
    fn test_parse_gpg_status_reads_both_fingerprints() {
        // Real shape of a subkey signature, as gpg 2.4 prints it.
        let raw = "[GNUPG:] NEWSIG\n\
                   [GNUPG:] GOODSIG 91044FBA76C9ED15AB14573589CAA03782C3251F\n\
                   [GNUPG:] VALIDSIG 91044FBA76C9ED15AB14573589CAA03782C3251F \
                   2026-09-26 1790441135 0 4 0 1 10 00 \
                   D34A9B0A08FFB8619ABEBC095D9B8736695F8C9D\n\
                   [GNUPG:] DECRYPTION_OKAY\n";
        let st = parse_gpg_status(raw);
        assert_eq!(st.bad_marker, None);
        assert!(st.decrypted_ok);
        // Field 1 is the signing subkey...
        assert_eq!(
            st.signing_key.as_deref(),
            Some("91044FBA76C9ED15AB14573589CAA03782C3251F")
        );
        // ...and the last field is the primary an operator would pin.
        assert_eq!(
            st.primary_key.as_deref(),
            Some("D34A9B0A08FFB8619ABEBC095D9B8736695F8C9D")
        );
    }

    #[test]
    fn test_parse_gpg_status_flags_died_signers() {
        // Every marker that means "this signature is not good" must be
        // caught, even when a VALIDSIG line is present alongside it.
        for marker in [
            "BADSIG 91044FBA76C9ED15AB14573589CAA03782C3251F",
            "ERRSIG 91044FBA76C9ED15AB14573589CAA03782C3251F 1 10 01 1789000000 9 0",
            "EXPSIG 91044FBA76C9ED15AB14573589CAA03782C3251F",
            "EXPKEYSIG 91044FBA76C9ED15AB14573589CAA03782C3251F",
            "REVKEYSIG 91044FBA76C9ED15AB14573589CAA03782C3251F",
            "KEYREVOKED 91044FBA76C9ED15AB14573589CAA03782C3251F",
        ] {
            let raw = format!(
                "[GNUPG:] VALIDSIG 91044FBA76C9ED15AB14573589CAA03782C3251F \
                 2026-09-26 1790441135 0 4 0 1 10 00 \
                 D34A9B0A08FFB8619ABEBC095D9B8736695F8C9D\n\
                 [GNUPG:] {marker}\n[GNUPG:] DECRYPTION_OKAY\n"
            );
            let st = parse_gpg_status(&raw);
            assert_eq!(st.bad_marker.as_deref(), Some(marker), "must flag {marker}");
            // The fingerprint is still parsed, so the error can name it.
            assert!(st.signing_key.is_some());
            assert!(st.decrypted_ok);
        }
    }

    #[test]
    fn test_parse_gpg_status_ignores_benign_noise() {
        // Encrypted-to lines and trust chatter must not look like failures.
        let raw = "[GNUPG:] ENC_TO 91044FBA76C9ED15AB14573589CAA03782C3251F 1 0\n\
                   [GNUPG:] BEGIN_DECRYPTION\n\
                   [GNUPG:] TRUST_UNDEFINED 0 shell\n\
                   [GNUPG:] DECRYPTION_INFO 2 9 1\n\
                   [GNUPG:] DECRYPTION_OKAY\n";
        let st = parse_gpg_status(raw);
        assert_eq!(st.bad_marker, None);
        assert!(st.decrypted_ok);
        assert_eq!(st.signing_key, None, "no signature was reported");
    }

    /// The guard may only be armed on paths Oot owns. Arming over a
    /// pre-existing file or directory would delete the user's data on a
    /// clean refusal, so `arm` must refuse instead.
    #[test]
    fn test_plaintext_guard_refuses_preexisting_paths() {
        let dir = guard_tmp("preexist");
        let file = dir.join("notes.tar");
        let staging = dir.join("notes");
        std::fs::write(&file, "PRECIOUS").unwrap();
        std::fs::create_dir_all(staging.join("deep")).unwrap();
        std::fs::write(staging.join("deep/data.txt"), "IRREPLACEABLE").unwrap();

        // Refuses, naming the conflict.
        let err = PlaintextGuard::arm(vec![staging.clone(), file.clone()]).unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "must name the conflict: {err}"
        );
        // And the refusal destroys nothing.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "PRECIOUS");
        assert_eq!(
            std::fs::read_to_string(staging.join("deep/data.txt")).unwrap(),
            "IRREPLACEABLE"
        );

        // A pre-existing symlink is refused by the same check.
        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.join("elsewhere"), &link).unwrap();
        let err = PlaintextGuard::arm(vec![link.clone()]).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "must say symlink: {err}"
        );
        assert!(link.symlink_metadata().is_ok(), "symlink must survive");

        // Only when both are absent does arming succeed, and dropping the
        // guard then removes exactly what Oot created.
        let created = dir.join("fresh");
        let tar = dir.join("fresh.tar");
        {
            let _guard = PlaintextGuard::arm(vec![created.clone(), tar.clone()]).unwrap();
            std::fs::create_dir_all(&created).unwrap();
            std::fs::write(created.join("x.txt"), "x").unwrap();
            std::fs::write(&tar, "tar").unwrap();
        }
        assert!(!created.exists(), "guard must remove the staging dir");
        assert!(!tar.exists(), "guard must remove the tar");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cleanup failure must be reportable: `Drop` cannot propagate, so
    /// `remove_now` exists and must return Err rather than warn into
    /// nothing. A path that is not removable in this process is simulated
    /// by pointing the guard at a path inside a file (ENOTDIR).
    #[cfg(unix)]
    #[test]
    fn test_plaintext_guard_reports_cleanup_failure() {
        let dir = guard_tmp("failclean");
        // A read-only parent: an unprivileged owner still cannot unlink
        // entries from a 0555 directory, so cleanup fails for real without
        // needing a second uid.
        let ro = dir.join("readonly");
        std::fs::create_dir_all(&ro).unwrap();
        let staging = ro.join("staging");
        let tar = ro.join("staging.tar");

        // arm() passes (both absent), the plaintext is created while the
        // parent is still writable, then the parent goes read-only so
        // cleanup fails for real.
        let guard = PlaintextGuard::arm(vec![staging.clone(), tar.clone()]).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("secret.txt"), "PLAINTEXT").unwrap();
        std::fs::write(&tar, "PLAINTEXT TAR").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        let err = guard.remove_now().unwrap_err();
        // Restore write access so the scratch dir can be cleaned up.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(
            err.to_string().contains("cleanup failed"),
            "must name the cleanup failure: {err}"
        );

        // A missing path is not a failure: nothing left to remove.
        let guard = PlaintextGuard::arm(vec![dir.join("never-existed")]).unwrap();
        assert!(guard.remove_now().is_ok(), "absent path is not a failure");

        let _ = std::fs::remove_dir_all(&dir);
    }

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
    fn test_commit_tree_cmd_sign_flag() {
        let tmp = std::env::temp_dir().join(format!("oot-sign-flag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let project = tmp.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let store = Store::init(&project).unwrap();
        let ident = Identity {
            name: "K".into(),
            email: "k@oot.dev".into(),
            time: 0,
            offset: "+0000".into(),
        };
        let record = ChangeRecord {
            parents: vec![],
            tree: "abc".into(),
            author: ident.clone(),
            committer: ident,
            message: "m\n".into(),
            source_sha: None,
        };
        let plain = format!("{:?}", store.commit_tree_cmd("abc", &record, None));
        assert!(
            !plain.contains("-S"),
            "unsigned build must not sign: {plain}"
        );
        let signed = format!("{:?}", store.commit_tree_cmd("abc", &record, Some("KEYID")));
        assert!(
            signed.contains("-SKEYID"),
            "key must reach commit-tree: {signed}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_commit_had_sig_only_reads_headers() {
        let tmp = std::env::temp_dir().join(format!("oot-sig-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let project = tmp.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let store = Store::init(&project).unwrap();

        let write_commit = |raw: &str| -> String {
            let mut child = std::process::Command::new("git")
                .args(["--git-dir"])
                .arg(store.git_dir())
                .args(["hash-object", "-t", "commit", "-w", "--stdin"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("hash-object has stdin")
                .write_all(raw.as_bytes())
                .unwrap();
            let done = child.wait_with_output().unwrap();
            assert!(done.status.success());
            String::from_utf8(done.stdout).unwrap().trim().to_string()
        };

        // Signed header, unsigned body mentioning gpgsig: still signed.
        let signed = write_commit(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor K <k@oot.dev> 0 +0000\ncommitter K <k@oot.dev> 0 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n iQDummy\n =sig\n -----END PGP SIGNATURE-----\n\nplain body\n",
        );
        assert!(store.commit_had_sig(&signed).unwrap());

        // sha256 variant counts too.
        let signed256 = write_commit(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor K <k@oot.dev> 0 +0000\ncommitter K <k@oot.dev> 0 +0000\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n iQDummy\n =sig\n -----END PGP SIGNATURE-----\n\nplain body\n",
        );
        assert!(store.commit_had_sig(&signed256).unwrap());

        // Body line starting with gpgsig must not fake a signature.
        let fake = write_commit(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor K <k@oot.dev> 0 +0000\ncommitter K <k@oot.dev> 0 +0000\n\ngpgsig not a real header\n",
        );
        assert!(!store.commit_had_sig(&fake).unwrap());

        // Plain unsigned commit.
        let plain_commit = write_commit(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor K <k@oot.dev> 0 +0000\ncommitter K <k@oot.dev> 0 +0000\n\nhello\n",
        );
        assert!(!store.commit_had_sig(&plain_commit).unwrap());

        // Missing object fails loud, never silent false.
        assert!(store
            .commit_had_sig("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
            .is_err());

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
