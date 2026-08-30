//! `oot update` — materialize a stored change's tree into the working copy.
//!
//! This module owns the whole `Update` flow so `main.rs` stays as dispatch.
//! It does not move branch pointers; `oot record` saves the result.

use anyhow::Context;
use oot::store::{validate_tree_path, Store};
use std::path::Path;

/// Run `oot update` with the parsed CLI flags.
pub fn run(
    change: Option<String>,
    branch: Option<String>,
    force: bool,
    dry_run: bool,
) -> anyhow::Result<std::process::ExitCode> {
    let root = std::env::current_dir()?;
    let store = Store::open(&root)?;

    // Resolve target tree.
    let is_change_target = change.is_some();
    let (target_tree, target_desc) = if let Some(c) = change {
        let id = store.resolve_change(&c)?;
        let rec = store.get_change(&id)?;
        (rec.tree, format!("change {}", &id[..7.min(id.len())]))
    } else {
        let b = crate::resolve_branch(&store, branch.clone())?;
        let hid = store
            .head_id(&b)?
            .ok_or_else(|| anyhow::anyhow!("branch '{b}' has no changes"))?;
        let rec = store.get_change(&hid)?;
        (
            rec.tree,
            format!("branch {b} @ {}", &hid[..7.min(hid.len())]),
        )
    };

    // Current head for dirty check — only require branch resolution for branch targets.
    // When --change is used (branch==None), try to infer head without requiring --branch:
    // single-branch stores can distinguish clean vs dirty work, multi-branch falls back
    // to work vs target direct check.
    let (current_branch_opt, current_tree) = if is_change_target {
        let inferred = match store.refs() {
            Ok(refs) if refs.len() == 1 => store
                .head_id(&refs[0].0)
                .ok()
                .flatten()
                .and_then(|h| store.get_change(&h).ok())
                .map(|r| r.tree)
                .unwrap_or_default(),
            _ => String::new(),
        };
        (None, inferred)
    } else {
        let current_branch = crate::resolve_branch(&store, branch.clone())?;
        let current_head_id = store.head_id(&current_branch)?;
        let current_tree = if let Some(ref hid) = current_head_id {
            store.get_change(hid)?.tree
        } else {
            String::new()
        };
        (Some(current_branch), current_tree)
    };

    let files = crate::collect_worktree(&root)?;
    let mut work: std::collections::HashMap<String, (String, bool)> =
        std::collections::HashMap::new();
    for f in &files {
        work.insert(f.path.clone(), (store.blob_sha(&f.contents)?, f.executable));
    }

    // Prepare target data before dirty check so dirty == work != target.
    let target_files = store.tree_files(&target_tree)?;
    let snapshot = store.snapshot_from_tree(&target_tree)?;

    // Validate every path from the tree before touching the filesystem.
    // Git ls-tree is verbatim; a crafted tree could contain `..`, absolute, or `//`.
    {
        for path in snapshot.files.keys().chain(target_files.keys()) {
            validate_tree_path(path)?;
            // Defense-in-depth: even after join, path must stay inside root.
            let full = root.join(path);
            if !full.starts_with(&root) {
                anyhow::bail!("invalid path in tree: '{}': escapes repository root", path);
            }
        }
    }

    // Consistency: work comes from collect_worktree (filtered via .gitignore)
    // but tree_files/snapshot are verbatim. For dirty detection we compare
    // filtered views so an ignored file like junk.log does not make status
    // and update disagree. Materialization below still writes verbatim
    // (including ignored files) — status will then hide them.
    let head_files = if current_tree.is_empty() {
        std::collections::HashMap::new()
    } else {
        store.tree_files(&current_tree)?
    };
    // Build filtered views matching collect_worktree's ignore rules.
    // One helper so status and update can never drift.
    let (same_git_root, ignore_rules) = crate::build_ignore_state(&root);
    let head_filtered = crate::filtered_tree(&head_files, &root, same_git_root, &ignore_rules);
    let target_filtered = crate::filtered_tree(&target_files, &root, same_git_root, &ignore_rules);
    let work_is_clean = work == head_filtered;
    let dirty = work != target_filtered;

    // Already up to date: no diff between filtered work and filtered target — skip rewriting.
    if work == target_filtered {
        if dry_run {
            println!("would update to {target_desc}: already up to date");
            return Ok(std::process::ExitCode::SUCCESS);
        }
        println!("already up to date");
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Dirty working copy handling:
    // - --force = old school: nuke work and materialize target verbatim
    // - clean work (work == head) = fast-forward without force, no merge needed
    // - dirty + no force = 3-way merge (base=head, ours=work, theirs=target)
    //   No data loss, keep work changes where target didn't touch the same path.
    //   Conflicts keep work and warn.
    let needs_merge = dirty && !work_is_clean && !force;

    if dry_run {
        if needs_merge {
            return dry_run_merge(&work, &head_filtered, &target_filtered, &target_desc);
        }
        // clean or --force dry-run: simple diff work vs target
        let mut added = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();
        for (path, sha_mode) in &target_filtered {
            match work.get(path) {
                None => added.push(path.clone()),
                Some(cur) if cur != sha_mode => modified.push(path.clone()),
                Some(_) => {}
            }
        }
        for path in work.keys() {
            if !target_filtered.contains_key(path) {
                deleted.push(path.clone());
            }
        }
        for v in [&mut added, &mut modified, &mut deleted] {
            v.sort();
        }
        if added.is_empty() && modified.is_empty() && deleted.is_empty() {
            println!("would update to {target_desc}: already up to date");
        } else {
            println!("would update to {target_desc}:");
            for (tag, paths) in [("A", &added), ("M", &modified), ("D", &deleted)] {
                for p in paths {
                    println!("  {tag} {p}");
                }
            }
            if dirty && !work_is_clean {
                println!("(use --force to discard local changes instead of merging)");
            }
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    if needs_merge {
        return materialize_merge(
            &root,
            &store,
            &work,
            &head_filtered,
            &target_filtered,
            &target_files,
            &snapshot,
            &target_desc,
            &files,
        );
    }

    // Clean or --force path falls through to normal materialize-to-target below.
    // If dirty && !work_is_clean && !force we already returned via merge above,
    // so reaching here means either work_is_clean or force.
    if dirty && !work_is_clean && !force {
        // This should be unreachable due to needs_merge check above, but keep
        // a loud bail for safety if logic drifts.
        let hint = if is_change_target {
            format!("working copy is dirty; use --force to overwrite (target {target_desc})")
        } else {
            let cur = current_branch_opt.as_deref().unwrap_or("unknown");
            format!("working copy is dirty; use --force to overwrite (target {target_desc} differs from {cur})")
        };
        anyhow::bail!(hint);
    }

    // Materialize: delete files not in target, then write target files.
    // Atomicity: deletes propagate errors (except NotFound); writes use
    // temp-file + rename so a crash never leaves a truncated file.
    // --force is required to delete untracked files not in target
    // (enforced by the dirty gate above); without --force we never
    // delete precious untracked work.
    //
    // .ootkeep / .oothave: marker files that keep a placeholder dir alive.
    // If work contains `logs/.ootkeep` and target has no `logs/`, we keep the
    // marker (and thus the dir) instead of deleting it. This lets you keep
    // empty dirs like `logs/` or `tmp/` across updates.
    for path in work.keys() {
        if path.ends_with(".ootkeep") || path.ends_with(".oothave") {
            continue;
        }
        if !target_files.contains_key(path) {
            let p = root.join(path);
            // Never touch .oot/.git/.jj — work already excludes them.
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => anyhow::bail!("failed to delete {}: {e}", p.display()),
            }
            // Remove empty parent dirs up to root (but never .oot/.git/.jj or root itself).
            if let Some(parent) = p.parent() {
                let mut cur = parent;
                while cur != root
                    && cur
                        .file_name()
                        .is_some_and(|n| n != ".oot" && n != ".git" && n != ".jj")
                {
                    // Lstat: if cur is a symlink, remove the symlink itself rather than following it.
                    if let Ok(meta) = std::fs::symlink_metadata(cur) {
                        if meta.file_type().is_symlink() {
                            let _ = std::fs::remove_file(cur);
                            if let Some(par) = cur.parent() {
                                cur = par;
                            } else {
                                break;
                            }
                            continue;
                        }
                    }
                    // Keep placeholder dirs that hold a keep marker.
                    if cur.join(".ootkeep").exists() || cur.join(".oothave").exists() {
                        break;
                    }
                    let empty = std::fs::read_dir(cur)
                        .map(|mut it| it.next().is_none())
                        .unwrap_or(false);
                    if empty {
                        match std::fs::remove_dir(cur) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                            Err(e) => {
                                anyhow::bail!("failed to remove dir {}: {e}", cur.display())
                            }
                        }
                        if let Some(par) = cur.parent() {
                            cur = par;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    }

    for (path, contents) in &snapshot.files {
        // Preserve exec bits from tree.
        let executable = target_files.get(path).map(|(_, e)| *e).unwrap_or(false);
        let full = root.join(path);
        // Never follow symlinks — lstat and remove symlink at target path.
        if let Ok(meta) = std::fs::symlink_metadata(&full) {
            if meta.file_type().is_symlink() {
                match std::fs::remove_file(&full) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        anyhow::bail!("failed to remove symlink {}: {e}", full.display())
                    }
                }
            }
        }
        if let Some(parent) = full.parent() {
            // Ensure no parent component is a symlink escaping the repo.
            if let Ok(rel) = parent.strip_prefix(&root) {
                let mut cur = root.clone();
                for comp in rel.components() {
                    cur.push(comp);
                    if let Ok(meta) = std::fs::symlink_metadata(&cur) {
                        if meta.file_type().is_symlink() {
                            match std::fs::remove_file(&cur) {
                                Ok(()) => {}
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(e) => {
                                    anyhow::bail!("failed to remove symlink {}: {e}", cur.display())
                                }
                            }
                        }
                    }
                }
            }
            std::fs::create_dir_all(parent)?;
        }
        // Atomic write via temp file + rename.
        let dir = full.parent().unwrap_or(Path::new(&root));
        let tmp_name = format!(
            ".oot-tmp-{}-{}",
            random_hex(),
            full.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string())
        );
        let tmp = dir.join(tmp_name);
        // If tmp is already a symlink, remove it before create_new to avoid symlink following.
        if let Ok(meta) = std::fs::symlink_metadata(&tmp) {
            if meta.file_type().is_symlink() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        // Use create_new + O_NOFOLLOW so a pre-existing symlink at tmp is never followed.
        // Fuck Windows — oot only targets Unix (Linux/macOS). This is the only path.
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            // O_NOFOLLOW = 0o400000 on Linux; raw constant avoids a libc dep.
            opts.custom_flags(0o400000);
            let mut f = opts
                .open(&tmp)
                .with_context(|| format!("failed to create temp file {}", tmp.display()))?;
            f.write_all(contents)
                .with_context(|| format!("failed to write temp file {}", tmp.display()))?;
        }
        if let Err(e) = std::fs::rename(&tmp, &full) {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!(
                "failed to rename {} -> {}: {e}",
                tmp.display(),
                full.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&full) {
                let mut perms = meta.permissions();
                perms.set_mode(if executable { 0o755 } else { 0o644 });
                let _ = std::fs::set_permissions(&full, perms);
            }
        }
    }

    println!("updated to {target_desc}");
    Ok(std::process::ExitCode::SUCCESS)
}

#[derive(Debug, PartialEq, Eq)]
enum MergeAction {
    TakeTarget,
    KeepWork,
    Conflict,
}

fn classify_merge(
    base: Option<&(String, bool)>,
    ours: Option<&(String, bool)>,
    theirs: Option<&(String, bool)>,
) -> MergeAction {
    if ours == base {
        // work didn't change this path -> take target (could be add/modify/delete)
        MergeAction::TakeTarget
    } else if theirs == base {
        // target didn't change -> keep work (preserves dirty work)
        MergeAction::KeepWork
    } else if ours == theirs {
        // both changed same way
        MergeAction::TakeTarget
    } else {
        // both changed differently
        MergeAction::Conflict
    }
}

fn dry_run_merge(
    work: &std::collections::HashMap<String, (String, bool)>,
    head_filtered: &std::collections::HashMap<String, (String, bool)>,
    target_filtered: &std::collections::HashMap<String, (String, bool)>,
    target_desc: &str,
) -> anyhow::Result<std::process::ExitCode> {
    use std::collections::{BTreeSet, HashSet};
    let mut all: HashSet<&String> = HashSet::new();
    for k in head_filtered
        .keys()
        .chain(work.keys())
        .chain(target_filtered.keys())
    {
        all.insert(k);
    }
    let mut kept = Vec::new();
    let mut taken = Vec::new();
    let mut conflicts = Vec::new();
    for path in all {
        let base = head_filtered.get(path);
        let ours = work.get(path);
        let theirs = target_filtered.get(path);
        match classify_merge(base, ours, theirs) {
            MergeAction::TakeTarget => {
                // Only report if it actually changes work
                if ours != theirs {
                    taken.push(path.clone());
                }
            }
            MergeAction::KeepWork => kept.push(path.clone()),
            MergeAction::Conflict => conflicts.push(path.clone()),
        }
    }
    // Include pure deletes that are take-target deletes already counted as taken
    let mut all_sorted: BTreeSet<String> = BTreeSet::new();
    for p in taken.iter().chain(kept.iter()).chain(conflicts.iter()) {
        all_sorted.insert(p.clone());
    }
    if kept.is_empty() && taken.is_empty() && conflicts.is_empty() {
        println!("would update to {target_desc}: already up to date");
        return Ok(std::process::ExitCode::SUCCESS);
    }
    println!("would update to {target_desc} (3-way merge, work preserved):");
    taken.sort();
    kept.sort();
    conflicts.sort();
    for p in &taken {
        println!("  T {p}  (take target)");
    }
    for p in &kept {
        println!("  K {p}  (keep work)");
    }
    for p in &conflicts {
        println!("  C {p}  (conflict: keep work, target differs)");
    }
    if !conflicts.is_empty() {
        println!(
            "conflicts keep work version; run `oot record -m \"wip\"` to save before resolving"
        );
    }
    println!("(use --force to discard work and take target verbatim)");
    Ok(std::process::ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn materialize_merge(
    root: &Path,
    _store: &Store,
    work: &std::collections::HashMap<String, (String, bool)>,
    head_filtered: &std::collections::HashMap<String, (String, bool)>,
    target_filtered: &std::collections::HashMap<String, (String, bool)>,
    target_files: &std::collections::HashMap<String, (String, bool)>,
    snapshot: &oot::change::Snapshot,
    target_desc: &str,
    _work_files: &[oot::store::WorkFile],
) -> anyhow::Result<std::process::ExitCode> {
    use std::collections::HashSet;
    // Build quick lookup for work files that are keep-work: we just leave them.
    let mut all_paths: HashSet<String> = HashSet::new();
    for k in head_filtered
        .keys()
        .chain(work.keys())
        .chain(target_filtered.keys())
    {
        all_paths.insert(k.clone());
    }
    // Also include verbatim target paths that are ignored (not in filtered) — they
    // are always taken as verbatim (status hides them anyway).
    let mut keep_work: HashSet<String> = HashSet::new();
    let mut take_target: HashSet<String> = HashSet::new();
    let mut conflicts: Vec<String> = Vec::new();
    for path in &all_paths {
        let base = head_filtered.get(path);
        let ours = work.get(path);
        let theirs = target_filtered.get(path);
        match classify_merge(base, ours, theirs) {
            MergeAction::TakeTarget => {
                take_target.insert(path.clone());
            }
            MergeAction::KeepWork => {
                keep_work.insert(path.clone());
            }
            MergeAction::Conflict => {
                keep_work.insert(path.clone());
                conflicts.push(path.clone());
            }
        }
    }

    // 1) Delete: only delete work files that are take-target and target has no entry
    //    (i.e., target deleted it and work didn't change it).
    //    Keep-work files are never deleted, including .ootkeep markers.
    for path in work.keys() {
        if keep_work.contains(path) {
            continue;
        }
        if path.ends_with(".ootkeep") || path.ends_with(".oothave") {
            continue;
        }
        if !take_target.contains(path) {
            continue;
        }
        // take-target and work has it but target doesn't -> delete
        if !target_filtered.contains_key(path) && !target_files.contains_key(path) {
            // Check filtered first, but use verbatim existence for actual delete
            // If path is in target_filtered as None, it's deleted. If it's also not
            // in target_files verbatim, it's definitely deleted.
            // For safety, check target_files verbatim.
            let p = root.join(path);
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => anyhow::bail!("failed to delete {}: {e}", p.display()),
            }
            if let Some(parent) = p.parent() {
                let mut cur = parent;
                while cur != root
                    && cur
                        .file_name()
                        .is_some_and(|n| n != ".oot" && n != ".git" && n != ".jj")
                {
                    if cur.join(".ootkeep").exists() || cur.join(".oothave").exists() {
                        break;
                    }
                    if let Ok(meta) = std::fs::symlink_metadata(cur) {
                        if meta.file_type().is_symlink() {
                            let _ = std::fs::remove_file(cur);
                            if let Some(par) = cur.parent() {
                                cur = par;
                            } else {
                                break;
                            }
                            continue;
                        }
                    }
                    let empty = std::fs::read_dir(cur)
                        .map(|mut it| it.next().is_none())
                        .unwrap_or(false);
                    if empty {
                        match std::fs::remove_dir(cur) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                            Err(e) => anyhow::bail!("failed to remove dir {}: {e}", cur.display()),
                        }
                        if let Some(par) = cur.parent() {
                            cur = par;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        } else if target_filtered.contains_key(path) {
            // work has it, target also has it but take_target means we will overwrite below
            // deletion not needed
        } else {
            // work has ignored file not in filtered target but maybe in verbatim target?
            // If verbatim target doesn't have it and it's take_target, we already deleted above.
        }
    }

    // Also handle work files that are take-target deletes where path not in work filtered
    // but in work verbatim? Work is already filtered, so ignored files not considered.

    // 2) Write: only write target files where decision is take-target.
    //    Keep-work paths are left alone.
    //    Also write verbatim ignored target files that are not in filtered decision
    //    (they are not in all_paths, but are in snapshot). Those are always written
    //    as per original verbatim behavior, unless keep_work says to keep work's version
    //    (but work doesn't have ignored files, so no conflict).
    let work_keep_set = keep_work;
    for (path, contents) in &snapshot.files {
        // If this path is a filtered path that we decided to keep work, skip writing target.
        if work_keep_set.contains(path) {
            continue;
        }
        // For filtered paths where take_target, we write. For ignored verbatim paths
        // not in filtered union, they were not classified; we always write them as before.
        // To know if a verbatim path is a filtered keep, check the filtered decision:
        // if path is in work_keep_set, skip. Otherwise write.
        // For paths not in filtered union, work_keep_set doesn't contain them, so we write.

        // But also need to respect take_target set: if path is filtered and not in take_target,
        // it means it was keep or conflict, already skipped above. So only write if
        // either it's not a filtered path, or it's in take_target.
        let is_filtered_path = target_filtered.contains_key(path)
            || head_filtered.contains_key(path)
            || work.contains_key(path);
        if is_filtered_path && !take_target.contains(path) {
            continue;
        }

        let executable = target_files.get(path).map(|(_, e)| *e).unwrap_or(false);
        let full = root.join(path);
        if let Ok(meta) = std::fs::symlink_metadata(&full) {
            if meta.file_type().is_symlink() {
                match std::fs::remove_file(&full) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => anyhow::bail!("failed to remove symlink {}: {e}", full.display()),
                }
            }
        }
        if let Some(parent) = full.parent() {
            if let Ok(rel) = parent.strip_prefix(root) {
                let mut cur = root.to_path_buf();
                for comp in rel.components() {
                    cur.push(comp);
                    if let Ok(meta) = std::fs::symlink_metadata(&cur) {
                        if meta.file_type().is_symlink() {
                            match std::fs::remove_file(&cur) {
                                Ok(()) => {}
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(e) => {
                                    anyhow::bail!("failed to remove symlink {}: {e}", cur.display())
                                }
                            }
                        }
                    }
                }
            }
            std::fs::create_dir_all(parent)?;
        }
        let dir = full.parent().unwrap_or(Path::new(root));
        let tmp_name = format!(
            ".oot-tmp-{}-{}",
            random_hex(),
            full.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string())
        );
        let tmp = dir.join(tmp_name);
        if let Ok(meta) = std::fs::symlink_metadata(&tmp) {
            if meta.file_type().is_symlink() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            opts.custom_flags(0o400000);
            let mut f = opts
                .open(&tmp)
                .with_context(|| format!("failed to create temp file {}", tmp.display()))?;
            f.write_all(contents)
                .with_context(|| format!("failed to write temp file {}", tmp.display()))?;
        }
        if let Err(e) = std::fs::rename(&tmp, &full) {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!(
                "failed to rename {} -> {}: {e}",
                tmp.display(),
                full.display()
            );
        }
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&full) {
                let mut perms = meta.permissions();
                perms.set_mode(if executable { 0o755 } else { 0o644 });
                let _ = std::fs::set_permissions(&full, perms);
            }
        }
    }

    if !conflicts.is_empty() {
        conflicts.sort();
        eprintln!(
            "merge conflicts (kept work version): {}",
            conflicts.join(", ")
        );
        eprintln!("hint: `oot record -m \"wip before {target_desc}\"` to save work, then resolve");
    }
    // Report kept work
    let kept_count = work_keep_set.len();
    if kept_count > 0 && conflicts.is_empty() {
        println!(
            "updated to {target_desc} (merged, kept {} work file(s))",
            kept_count
        );
    } else if !conflicts.is_empty() {
        println!(
            "updated to {target_desc} (merged with {} conflict(s), kept work)",
            conflicts.len()
        );
    } else {
        println!("updated to {target_desc} (merged)");
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// 16-hex-char random suffix for temp files.
/// Tries /dev/urandom first (Linux), falls back to a hash of time+pid+thread.
fn random_hex() -> String {
    let mut buf = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    // Fallback: hash time + pid + thread id. Not crypto-random, but
    // unpredictable enough for a temp name and we still use create_new.
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    std::process::id().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    format!("{:016x}", h.finish())
}
