//! Command-line entry point for Oot.
//!
//! Adjudicates changes across snapshots against meaning and visibility policies.

use clap::{Parser, Subcommand};
use oot::adapter::{GitAdapter, GitAdjudicateOptions, JjAdapter, JjAdjudicateOptions};
use oot::change::{Change, Snapshot, Source};
use oot::dispute::{finalize_adjudication, Docket, Kind, Severity, Verdict};
use oot::docket;
use oot::engine::Engine;
use oot::policy::MeaningPolicy;
use oot::visibility::VisibilityPolicy;

/// Command-line parser for the Oot CLI.
#[derive(Parser)]
#[command(name = "oot", about = "Git settles lines. Oot settles meaning.")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Available CLI subcommands.
#[derive(Subcommand)]
enum Commands {
    /// Adjudicate a Change and print its docket.
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
