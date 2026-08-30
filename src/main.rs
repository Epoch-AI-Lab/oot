//! Command-line entry point for Oot.
//!
//! Adjudicates changes across snapshots against meaning and visibility policies.

use anyhow::Context;
use clap::{Parser, Subcommand};
use oot::adapter::{GitAdapter, GitAdjudicateOptions, JjAdapter, JjAdjudicateOptions};
use oot::change::{Change, Snapshot, Source};
use oot::court;
use oot::dispute::{finalize_adjudication, Docket, Kind, Severity, Verdict};
use oot::docket;
use oot::engine::Engine;
use oot::policy::MeaningPolicy;
use oot::visibility::VisibilityPolicy;
use std::process::Command;

use oot::store::Store;

mod update;

/// Command-line parser for the Oot CLI.
#[derive(Parser)]
#[command(
    name = "oot",
    version,
    about = "Git settles lines. Oot settles meaning."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Available CLI subcommands.
// Built exactly once at startup; variant size is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Commands {
    /// Adjudicate a Change and print its docket.
    #[command(alias = "court")]
    Adjudicate {
        /// Change name or identifier.
        #[arg(long)]
        change: Option<String>,
        /// Where the change came from: git, jj, or memory.
        #[arg(long)]
        source: Option<String>,
        /// Base snapshot directory.
        #[arg(long)]
        base: Option<String>,
        /// Head snapshot directory.
        #[arg(long)]
        head: Option<String>,
        /// Git base reference (e.g. `main` or commit SHA).
        #[arg(long)]
        base_ref: Option<String>,
        /// Git head reference (e.g. `feature/auth` or commit SHA).
        #[arg(long)]
        head_ref: Option<String>,
        /// Explicit merge-base (git) or common-ancestor (jj) commit override for 3-way adjudication.
        #[arg(long)]
        merge_base: Option<String>,
        /// Path to the Git repository root (defaults to discovering from current directory).
        #[arg(long)]
        repo: Option<String>,
        /// Comma-separated authors.
        #[arg(long)]
        authors: Option<String>,
        /// Stated intent or purpose of the change.
        #[arg(long)]
        intent: Option<String>,
        /// Path to a meaning-policy TOML file.
        #[arg(long)]
        policy: Option<String>,
        /// Path to a visibility-policy TOML file.
        #[arg(long)]
        visibility: Option<String>,
        /// Load and print a previously saved docket instead of adjudicating.
        #[arg(long)]
        docket: Option<String>,
        /// Path to save the resulting docket as JSON.
        #[arg(long, short = 'o')]
        output: Option<String>,
        /// Store mode only: skip persisting the docket into `.oot/dockets/`
        /// and the `.oot/adjudications.jsonl` audit trail.
        #[arg(long)]
        no_save: bool,
    },
    /// Initialize an Oot store (`.oot/`) in the current project.
    Init,
    /// Import git history from a repository into the Oot store.
    Import {
        /// Source git repository (defaults to discovering from the current directory).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Capture the working copy as a new change in the store — no git
    /// history involved. This is Oot's own write path.
    Record {
        /// Change message.
        #[arg(long, short)]
        message: String,
        /// Branch to record onto (defaults to the store's only branch, or `main`).
        #[arg(long)]
        branch: Option<String>,
    },
    /// Show what a `record` would capture: working-copy deltas vs the
    /// branch head.
    Status {
        /// Branch to compare against (defaults to the store's only branch, or `main`).
        #[arg(long)]
        branch: Option<String>,
    },
    /// List a branch's changes, newest first.
    Log {
        /// Branch whose history to show (defaults to the store's only branch, or `main`).
        #[arg(long)]
        branch: Option<String>,
    },
    /// Export the store's history as a plain git repository ready for push.
    Export {
        /// Directory to create the exported repository in.
        #[arg(long)]
        out: String,
        /// Path to a visibility-policy TOML. Defaults to `./visibility.toml`
        /// when present; with no policy found, export is unfiltered and
        /// byte-exact.
        #[arg(long)]
        visibility: Option<String>,
    },
    /// Render a docket persisted by a previous `oot adjudicate --change`.
    Docket {
        /// Change id or unique prefix.
        #[arg(value_name = "ID")]
        id: Option<String>,
        /// Change id or unique prefix.
        #[arg(long)]
        change: Option<String>,
    },
    /// Materialize a stored change's tree into the working copy.
    /// Does not move any branch pointer. Run `oot record` to save the result as a new change.
    Update {
        /// Change id or unique prefix to materialize.
        #[arg(long, conflicts_with = "branch")]
        change: Option<String>,
        /// Branch whose head to materialize (defaults to the store's only branch, or `main`).
        #[arg(long, conflicts_with = "change")]
        branch: Option<String>,
        /// Overwrite local changes and delete untracked files not in target when the working copy is dirty.
        #[arg(long)]
        force: bool,
        /// Preview what would change without touching the working copy. Ignored files are hidden (matches status).
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Adjudicate {
            change,
            source,
            base,
            head,
            base_ref,
            head_ref,
            merge_base,
            repo,
            authors,
            intent,
            policy,
            visibility,
            docket,
            output,
            no_save,
        } => {
            if let Some(path) = docket {
                let d = docket::load(std::path::Path::new(&path))?;
                print!("{}", d.render());
                return Ok(std::process::ExitCode::SUCCESS);
            }

            let meaning_policy = match policy {
                Some(p) => MeaningPolicy::load(std::path::Path::new(&p))?,
                None => MeaningPolicy::default(),
            };
            let visibility_policy = match visibility {
                Some(v) => VisibilityPolicy::load(std::path::Path::new(&v))?,
                None => VisibilityPolicy::default(),
            };
            let eng = Engine::new()?;

            // Store-backed adjudication: `--change <id|prefix>` engages only
            // when an Oot store opens here and no other adjudication mode was
            // requested; otherwise the existing modes below behave untouched.
            // Once engaged, an unresolvable id fails loudly rather than
            // silently falling through.
            let other_mode = source.is_some()
                || base.is_some()
                || head.is_some()
                || base_ref.is_some()
                || head_ref.is_some()
                || repo.is_some()
                || docket.is_some();
            let store = match (change.clone(), other_mode) {
                (Some(_), false) => Store::open(".").ok(),
                _ => None,
            };
            if let Some(store) = store {
                let change_flag = change.as_deref().expect("checked above");
                let id = store.resolve_change(change_flag)?;
                let authors_list = authors.as_ref().map(|a| {
                    a.split(',')
                        .map(|s| s.trim().to_string())
                        .collect::<Vec<_>>()
                });
                let persisted = court::adjudicate_change(
                    &store,
                    &id,
                    &eng,
                    &meaning_policy,
                    &visibility_policy,
                    intent.clone(),
                    authors_list,
                )?;
                print!("{}", persisted.docket.render());
                if !no_save {
                    court::save_docket(&store, &persisted)?;
                    court::log_adjudication(&store, &persisted)?;
                }
                if let Some(out_path) = output {
                    docket::save(&persisted.docket, std::path::Path::new(&out_path))?;
                }
                return Ok(exit_code_for(persisted.docket.verdict));
            }

            // VCS 3-way In-Memory Adjudication (git or jj)
            if let (Some(b_ref), Some(h_ref)) = (base_ref, head_ref) {
                let wants_jj = matches!(source.as_deref(), Some("jj") | Some("jujutsu"));

                let doc = if wants_jj {
                    let jj_adapter = match repo {
                        Some(r) => JjAdapter::new(r)?,
                        None => JjAdapter::discover()?,
                    };

                    let options = JjAdjudicateOptions {
                        custom_ancestor: merge_base,
                        change_name: change,
                        intent,
                    };

                    jj_adapter.adjudicate_3way(
                        &b_ref,
                        &h_ref,
                        &eng,
                        &meaning_policy,
                        &visibility_policy,
                        &options,
                    )?
                } else {
                    let git_adapter = match repo {
                        Some(r) => GitAdapter::new(r)?,
                        None => GitAdapter::discover()?,
                    };

                    let options = GitAdjudicateOptions {
                        custom_merge_base: merge_base,
                        change_name: change,
                        intent,
                    };

                    git_adapter.adjudicate_3way(
                        &b_ref,
                        &h_ref,
                        &eng,
                        &meaning_policy,
                        &visibility_policy,
                        &options,
                    )?
                };

                print!("{}", doc.render());

                if let Some(out_path) = output {
                    docket::save(&doc, std::path::Path::new(&out_path))?;
                }
                return Ok(exit_code_for(doc.verdict));
            }

            // Materialized Directory Snapshot Adjudication
            let (base_dir, head_dir) = match (base, head) {
                (Some(b), Some(h)) => (b, h),
                _ => {
                    eprintln!(
                        "provide --docket <file>, --base <dir> --head <dir>, or --base-ref <ref> --head-ref <ref>"
                    );
                    std::process::exit(2);
                }
            };

            let mut base_snap = Snapshot::default();
            load_dir(
                std::path::Path::new(&base_dir),
                std::path::Path::new(&base_dir),
                &mut base_snap.files,
            )?;
            let mut head_snap = Snapshot::default();
            load_dir(
                std::path::Path::new(&head_dir),
                std::path::Path::new(&head_dir),
                &mut head_snap.files,
            )?;

            let change = Change {
                name: change.unwrap_or_else(|| "unnamed".into()),
                source: source.unwrap_or_else(|| "git".into()).parse::<Source>()?,
                base_ref: base_dir.clone(),
                head_ref: head_dir.clone(),
                base: base_snap,
                head: head_snap,
                authors: authors
                    .map(|a| a.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_else(|| vec!["@you".into()]),
                intent: intent.clone(),
            };

            let vis_disputes = visibility_policy.check(&change);
            let cloaked = vis_disputes
                .iter()
                .any(|d| d.kind == Kind::Visibility && d.severity == Severity::High);
            let mut disputes = eng.diff_snapshots(&change.base, &change.head)?;
            disputes.extend(vis_disputes);

            let (disputes, intent, verdict) = finalize_adjudication(
                disputes,
                &change.base.files,
                &change.head.files,
                intent.clone(),
                cloaked,
                visibility_policy.embargo_until.is_some(),
                &meaning_policy,
            );

            let docket = Docket {
                change: change.name.clone(),
                source: change.source.as_str().to_string(),
                base: change.base_ref.clone(),
                head: change.head_ref.clone(),
                disputes,
                intent,
                authors: change.authors.clone(),
                verdict,
                embargo: visibility_policy.embargo_note(),
            };

            print!("{}", docket.render());

            if let Some(out_path) = output {
                docket::save(&docket, std::path::Path::new(&out_path))?;
            }

            Ok(exit_code_for(docket.verdict))
        }
        Commands::Init => {
            let store = Store::init(".")?;
            println!("initialized Oot store at {}", store.path().display());
            Ok(std::process::ExitCode::SUCCESS)
        }
        Commands::Import { repo } => {
            let source = match repo {
                Some(r) => GitAdapter::new(&r)?,
                None => GitAdapter::discover()?,
            };
            let store = Store::open(".")?;
            let branches = store.fetch_branches(source.repo_root())?;
            if branches.is_empty() {
                anyhow::bail!("source repository has no branches");
            }

            for branch in &branches {
                let commits = store.log_raw(source.repo_root(), branch)?;
                if commits.is_empty() {
                    eprintln!("branch {branch}: no commits, skipped");
                    continue;
                }
                let mut head_id = String::new();
                for raw in &commits {
                    let id = store.put_commit(raw)?;
                    store.index_push(&id)?;
                    head_id = id;
                }
                store.set_ref(branch, &head_id)?;
                println!("branch {branch}: {} change(s) imported", commits.len());
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Commands::Record { message, branch } => {
            let root = std::env::current_dir()?;
            let store = Store::open(&root)?;
            let branch = resolve_branch(&store, branch)?;

            let (author, committer) = resolve_identity(&root)?;

            let files = collect_worktree(&root)?;
            let tree = store.write_tree_from_files(&files)?;

            let head_id = store.head_id(&branch)?;
            if let Some(ref head) = head_id {
                let parent_tree = store.get_change(head)?.tree;
                if parent_tree == tree {
                    anyhow::bail!("nothing to record: working copy matches {branch}'s head");
                }
            }
            let parents: Vec<String> = head_id.iter().cloned().collect();

            let kind = if head_id.is_some() {
                "child of head"
            } else {
                "root"
            };
            let record = oot::store::ChangeRecord {
                parents,
                tree,
                author,
                committer,
                message: ensure_trailing_newline(&message),
                source_sha: None,
            };
            let id = store.put_record(&record)?;
            store.index_push(&id)?;
            store.set_ref(&branch, &id)?;

            println!(
                "recorded {id} on {branch} as {kind}: {} file(s)",
                files.len()
            );
            println!("next: oot adjudicate --change {}", &id[..7]);
            Ok(std::process::ExitCode::SUCCESS)
        }
        Commands::Status { branch } => {
            let root = std::env::current_dir()?;
            let store = Store::open(&root)?;
            let branch = resolve_branch(&store, branch)?;
            let head_id = store.head_id(&branch)?;

            let files = collect_worktree(&root)?;
            let mut work: std::collections::HashMap<String, (String, bool)> =
                std::collections::HashMap::new();
            for f in &files {
                work.insert(f.path.clone(), (store.blob_sha(&f.contents)?, f.executable));
            }

            let mut added = Vec::new();
            let mut modified = Vec::new();
            let mut deleted = Vec::new();
            if let Some(head) = &head_id {
                let head_tree = store.get_change(head)?.tree;
                // Consistency: head is verbatim but work is filtered; filter head
                // the same way so ignored files (e.g. junk.log) don't show as
                // deleted and status agrees with update's dirty check.
                let (same_git_root, ignore_rules) = build_ignore_state(&root);
                let head_files = store.tree_files(&head_tree)?;
                let head_filtered = filtered_tree(&head_files, &root, same_git_root, &ignore_rules);
                for (path, sha_mode) in head_filtered {
                    match work.remove(&path) {
                        Some(current) if current != sha_mode => modified.push(path),
                        Some(_) => {}
                        None => deleted.push(path),
                    }
                }
                added.extend(work.into_keys());
            } else {
                added.extend(files.iter().map(|f| f.path.clone()));
            }
            for v in [&mut added, &mut modified, &mut deleted] {
                v.sort();
            }

            match (
                &head_id,
                added.is_empty() && modified.is_empty() && deleted.is_empty(),
            ) {
                (_, true) => match head_id.as_deref().map(|h| &h[..7.min(h.len())]) {
                    Some(id) => println!("branch {branch} @ {id}: up to date"),
                    None => println!("branch {branch}: empty store, nothing recorded yet"),
                },
                (Some(id), false) => {
                    println!(
                        "branch {branch} @ {id} — {} change(s) to record:",
                        added.len() + modified.len() + deleted.len()
                    );
                    print_deltas(&added, &modified, &deleted);
                }
                (None, false) => {
                    println!(
                        "branch {branch} has no changes yet — {} file(s) would be captured:",
                        added.len()
                    );
                    print_deltas(&added, &modified, &deleted);
                }
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Commands::Log { branch } => {
            let store = Store::open(".")?;
            let branch = resolve_branch(&store, branch)?;
            let head_id = store
                .head_id(&branch)?
                .ok_or_else(|| anyhow::anyhow!("branch '{branch}' has no changes"))?;

            // Reachable set from the head...
            let mut reachable: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut queue = vec![head_id];
            while let Some(id) = queue.pop() {
                if !reachable.insert(id.clone()) {
                    continue;
                }
                queue.extend(store.get_change(&id)?.parents.iter().cloned());
            }
            // ...printed newest first (index order is chronological).
            let mut stdout = std::io::stdout();
            use std::io::Write;
            for id in store.index()?.iter().rev() {
                if !reachable.contains(id) {
                    continue;
                }
                let record = store.get_change(id)?;
                let kind = if record.source_sha.is_some() {
                    "git"
                } else {
                    "oot"
                };
                // Piping into `head` closes stdout early; stop quietly
                // instead of panicking on the broken pipe.
                if writeln!(
                    stdout,
                    "{} {} {} {} [{}]",
                    &id[..7],
                    oot::store::format_date(&record.committer),
                    record.author.name,
                    record.message.lines().next().unwrap_or_default(),
                    kind
                )
                .is_err()
                {
                    break;
                }
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Commands::Export { out, visibility } => {
            let policy = match &visibility {
                Some(p) => Some(VisibilityPolicy::load(std::path::Path::new(p))?),
                None => {
                    let candidate = std::path::Path::new("visibility.toml");
                    if candidate.exists() {
                        Some(VisibilityPolicy::load(candidate)?)
                    } else {
                        None
                    }
                }
            };

            // An embargo holds every change: there is nothing safe to export.
            if let Some(date) = policy.as_ref().and_then(|p| p.embargo_until.as_deref()) {
                anyhow::bail!("export refused: store is under embargo until {date}");
            }

            let out_path = std::path::PathBuf::from(&out);
            if out_path.exists() {
                anyhow::bail!("export directory already exists: {out}");
            }
            std::fs::create_dir_all(&out_path)?;
            run_git(&["init", "--quiet"], &out_path)?;

            let store = Store::open(".")?;
            let refs = store.refs()?;
            if refs.is_empty() {
                anyhow::bail!("store has no imported history (run `oot import` first)");
            }
            store.replay(&out_path, policy.as_ref())?;

            for (branch, head_id) in &refs {
                match store.branch_head_sha(head_id)? {
                    Some(sha) => {
                        store.point_ref(&out_path, branch, &sha)?;
                        println!("branch {branch} -> {sha}");
                    }
                    None => {
                        store.log_branch_omitted(branch, head_id)?;
                        println!("branch {branch} omitted (entire history withheld)");
                    }
                }
            }

            // Point HEAD at the first branch so `git log` works immediately.
            let first = format!("refs/heads/{}", refs[0].0);
            run_git(&["symbolic-ref", "HEAD", &first], &out_path)?;

            println!(
                "exported to {out}\nnext: cd {out} && git remote add origin <url> && git push -u origin --all"
            );
            Ok(std::process::ExitCode::SUCCESS)
        }
        Commands::Docket { id, change } => {
            let target_id = change
                .or(id)
                .ok_or_else(|| anyhow::anyhow!("specify a change id or pass --change <id>"))?;
            let store = Store::open(".")?;
            let change_id = store.resolve_change(&target_id)?;
            let persisted = court::load_docket(&store, &change_id)?;
            print!("{}", persisted.docket.render());
            Ok(std::process::ExitCode::SUCCESS)
        }
        Commands::Update {
            change,
            branch,
            force,
            dry_run,
        } => update::run(change, branch, force, dry_run),
    }
}

fn run_git(args: &[&str], cwd: &std::path::Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Branch selection shared by record/status/log: explicit flag wins;
/// otherwise the store's only branch; otherwise a fresh store starts on main.
pub(crate) fn resolve_branch(store: &Store, flag: Option<String>) -> anyhow::Result<String> {
    match flag {
        Some(b) => Ok(b),
        None => match store.refs()?.as_slice() {
            [] => Ok("main".to_string()),
            [(name, _)] => Ok(name.clone()),
            refs => anyhow::bail!(
                "store has multiple branches; pass --branch <name> ({})",
                refs.iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
    }
}

fn print_deltas(added: &[String], modified: &[String], deleted: &[String]) {
    for (tag, paths) in [("A", added), ("M", modified), ("D", deleted)] {
        for path in paths {
            println!("  {tag} {path}");
        }
    }
    if !added.is_empty() || !modified.is_empty() || !deleted.is_empty() {
        println!("record with: oot record -m \"...\"");
    }
}

/// Author/committer identities for recorded changes. Git's env vars win
/// (same precedence git itself uses), then git config — so CI and scripted
/// use work without any global config. Fail loudly rather than fabricating.
fn resolve_identity(
    root: &std::path::Path,
) -> anyhow::Result<(oot::store::Identity, oot::store::Identity)> {
    let time = oot::store::now_epoch() as i64;
    let offset = local_offset()?;
    let author_name = env_or_config("GIT_AUTHOR_NAME", root, "user.name")?;
    let author_email = env_or_config("GIT_AUTHOR_EMAIL", root, "user.email")?;
    let committer_name = env_or_config("GIT_COMMITTER_NAME", root, "user.name")?;
    let committer_email = env_or_config("GIT_COMMITTER_EMAIL", root, "user.email")?;
    let mk = |name: String, email: String| oot::store::Identity {
        name,
        email,
        time,
        offset: offset.clone(),
    };
    Ok((
        mk(author_name, author_email),
        mk(committer_name, committer_email),
    ))
}

fn env_or_config(
    env_key: &str,
    root: &std::path::Path,
    config_key: &str,
) -> anyhow::Result<String> {
    if let Ok(value) = std::env::var(env_key) {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    git_config(root, config_key).ok_or_else(|| {
        anyhow::anyhow!("no identity found: set {env_key} or `git config --global {config_key}`")
    })
}

fn git_config(root: &std::path::Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--get", key])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// The machine's local timezone offset in git's raw form (e.g. `+0530`).
fn local_offset() -> anyhow::Result<String> {
    let out = Command::new("date")
        .arg("+%z")
        .output()
        .context("failed to run date %z")?;
    let offset = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let valid = offset.len() == 5
        && (offset.starts_with('+') || offset.starts_with('-'))
        && offset[1..].chars().all(|c| c.is_ascii_digit());
    if !valid {
        anyhow::bail!("unexpected timezone offset from date: '{offset}'");
    }
    Ok(offset)
}

/// Recursively collect working-copy files into [`WorkFile`]s. `.oot` and
/// `.git` are never captured; symlinks are never followed; when the project
/// sits at a git worktree root, `.gitignore` rules are honored best-effort.
pub(crate) fn collect_worktree(
    root: &std::path::Path,
) -> anyhow::Result<Vec<oot::store::WorkFile>> {
    fn walk(
        dir: &std::path::Path,
        rel: &str,
        out: &mut Vec<oot::store::WorkFile>,
    ) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let p = entry.path();
            // Never follow symlinks: a loop would recurse forever and an
            // escaping link would ingest content from outside the snapshot.
            if entry.file_type()?.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if p.is_dir() {
                if name == ".oot" || name == ".git" || name == ".jj" {
                    continue;
                }
                walk(&p, &child_rel, out)?;
            } else {
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(&p).map(|m| m.permissions().mode() & 0o111 != 0)?
                };
                out.push(oot::store::WorkFile {
                    path: child_rel,
                    contents: std::fs::read(&p)?,
                    executable,
                });
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(root, "", &mut files)?;

    // Ignore rules: inside a git worktree whose root matches ours, git
    // decides (full semantics). Pure-Oot projects fall back to a minimal
    // matcher over the root .gitignore — enough for names, directories,
    // and * globs; negations are not supported.
    let (same_git_root, ignore_rules) = build_ignore_state(root);
    files.retain(|f| !is_path_ignored(root, &f.path, same_git_root, &ignore_rules));
    Ok(files)
}

pub(crate) fn build_ignore_state(root: &std::path::Path) -> (bool, Vec<String>) {
    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let same_git_root = toplevel
        .map(|top| {
            let canonical_root = std::fs::canonicalize(root).ok();
            if let Some(canonical_root) = canonical_root {
                std::fs::canonicalize(std::path::Path::new(&top))
                    .map(|p| p == canonical_root)
                    .unwrap_or_else(|_| canonical_root.to_string_lossy() == top)
            } else {
                false
            }
        })
        .unwrap_or(false);
    let ignore_rules: Vec<String> = if same_git_root {
        Vec::new()
    } else {
        std::fs::read_to_string(root.join(".gitignore"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
            .collect()
    };
    (same_git_root, ignore_rules)
}

pub(crate) fn is_path_ignored(
    root: &std::path::Path,
    path: &str,
    same_git_root: bool,
    ignore_rules: &[String],
) -> bool {
    if same_git_root {
        is_git_ignored(root, path)
    } else {
        simple_ignored(path, ignore_rules)
    }
}

/// Filter a `tree_files` map the same way `collect_worktree` filters the
/// working copy: ignored paths are hidden so `status` and `update` agree.
/// The input map is verbatim from the store; the output is what the user
/// should see.
pub(crate) fn filtered_tree(
    tree: &std::collections::HashMap<String, (String, bool)>,
    root: &std::path::Path,
    same_git_root: bool,
    ignore_rules: &[String],
) -> std::collections::HashMap<String, (String, bool)> {
    tree.iter()
        .filter(|(p, _)| !is_path_ignored(root, p, same_git_root, ignore_rules))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Best-effort .gitignore subset for projects without git: exact names,
/// `dir/` prefixes, and `*` wildcards, matched per git's basename rule.
fn simple_ignored(rel: &str, rules: &[String]) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    rules.iter().any(|rule| {
        let anchored = rule.contains('/');
        let rule = rule.trim_end_matches('/');
        if anchored && !rule.is_empty() {
            wildcard_match(rule, rel)
                || rel
                    .strip_prefix(rule)
                    .is_some_and(|rest| rest.starts_with('/'))
        } else if rule.contains('*') {
            wildcard_match(rule, base)
        } else {
            base == rule || rel.split('/').any(|part| part == rule)
        }
    })
}

/// Glob matching with `*` only (no `?`, no character classes).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => text
            .strip_prefix(head)
            .is_some_and(|rest| rest.len() >= tail.len() && rest.ends_with(tail)),
    }
}

fn is_git_ignored(root: &std::path::Path, path: &str) -> bool {
    use std::io::Write;
    let child = Command::new("git")
        .args(["check-ignore", "-q", "--stdin"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn();
    // One process per path is slow but simple; batching can come later.
    match child {
        Ok(mut c) => {
            if let Some(stdin) = c.stdin.as_mut() {
                let _ = writeln!(stdin, "{path}");
            }
            drop(c.stdin.take());
            c.wait().map(|s| s.success()).unwrap_or(false)
        }
        Err(_) => false,
    }
}

fn ensure_trailing_newline(message: &str) -> String {
    if message.ends_with('\n') {
        message.to_string()
    } else {
        format!("{message}\n")
    }
}

/// Exit-code contract for `oot adjudicate`:
/// - `0`: verdict is `Adjudicated` — ship-ready.
/// - `1`: any other verdict (`Blocked`, `Cloaked`, `Embargoed`) — all mean
///   "do not ship yet", so CI and merge gates can treat any nonzero as a stop.
/// - `2`: usage error (reserved by the CLI parser paths).
fn exit_code_for(verdict: Verdict) -> std::process::ExitCode {
    if verdict == Verdict::Adjudicated {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// Recursively read files in a directory into a HashMap of relative paths to raw contents.
fn load_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    files: &mut std::collections::HashMap<String, Vec<u8>>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        // Never follow symlinks: a loop would recurse forever and an
        // escaping link would ingest content from outside the snapshot.
        if entry.file_type()?.is_symlink() {
            continue;
        }
        if p.is_dir() {
            load_dir(root, &p, files)?;
        } else {
            // Store raw bytes so binary files (images, lockfiles, etc.) are
            // tracked with exact content; text conversion happens at parse time.
            let bytes = std::fs::read(&p)?;
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(rel, bytes);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_ignored_matches_git_basics() {
        let rules: Vec<String> = ["junk.log", "target/", "*.tmp", "secrets/keys"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Basename rule at any depth.
        assert!(simple_ignored("junk.log", &rules));
        assert!(simple_ignored("deep/nested/junk.log", &rules));
        // Directory prefix rule.
        assert!(simple_ignored("target/debug/foo.rs", &rules));
        // Glob rule on basename.
        assert!(simple_ignored("cache/session.tmp", &rules));
        // Anchored slash rule.
        assert!(simple_ignored("secrets/keys", &rules));
        // Clean paths stay.
        assert!(!simple_ignored("src/main.rs", &rules));
        assert!(!simple_ignored("notjunk.log", &rules));
    }

    #[test]
    fn test_wildcard_match() {
        assert!(wildcard_match("*.log", "a.log"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("foo*", "foobar"));
        assert!(wildcard_match("*bar", "foobar"));
        assert!(!wildcard_match("*.log", "a.txt"));
        assert!(!wildcard_match("foo*", "barfoo"));
    }
}
