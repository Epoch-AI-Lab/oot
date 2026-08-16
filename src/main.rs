mod change;
mod dispute;
mod docket;
mod engine;
mod policy;
mod visibility;

use change::{Change, Snapshot, Source};
use clap::{Parser, Subcommand};
use dispute::{Docket, Kind, Severity, Verdict};

#[derive(Parser)]
#[command(name = "oot", about = "Git settles lines. Oot settles meaning.")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Adjudicate a Change and print its docket.
    Adjudicate {
        /// Change name.
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
        /// Comma-separated authors.
        #[arg(long)]
        authors: Option<String>,
        /// Path to a meaning-policy TOML file.
        #[arg(long)]
        policy: Option<String>,
        /// Path to a visibility-policy TOML file.
        #[arg(long)]
        visibility: Option<String>,
        /// Load and print a previously saved docket instead of adjudicating.
        #[arg(long)]
        docket: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Adjudicate {
            change,
            source,
            base,
            head,
            authors,
            policy,
            visibility,
            docket,
        } => {
            if let Some(path) = docket {
                let d = docket::load(std::path::Path::new(&path))?;
                print!("{}", d.render());
                return Ok(());
            }

            let (base_dir, head_dir) = match (base, head) {
                (Some(b), Some(h)) => (b, h),
                _ => {
                    eprintln!("provide --docket <file> or --base <dir> --head <dir>");
                    std::process::exit(2);
                }
            };

            let mut base_snap = Snapshot::default();
            load_dir(std::path::Path::new(&base_dir), std::path::Path::new(&base_dir), &mut base_snap.files)?;
            let mut head_snap = Snapshot::default();
            load_dir(std::path::Path::new(&head_dir), std::path::Path::new(&head_dir), &mut head_snap.files)?;

            let change = Change {
                name: change.unwrap_or_else(|| "unnamed".into()),
                source: source
                    .unwrap_or_else(|| "git".into())
                    .parse::<Source>()?,
                base_ref: base_dir.clone(),
                head_ref: head_dir.clone(),
                base: base_snap,
                head: head_snap,
                authors: authors
                    .map(|a| a.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_else(|| vec!["@you".into()]),
                intent: None,
            };

            let meaning_policy = match policy {
                Some(p) => policy::MeaningPolicy::load(std::path::Path::new(&p))?,
                None => policy::MeaningPolicy::default(),
            };
            let visibility_policy = match visibility {
                Some(v) => visibility::VisibilityPolicy::load(std::path::Path::new(&v))?,
                None => visibility::VisibilityPolicy::default(),
            };

            let eng = engine::Engine::new()?;
            let vis_disputes = visibility_policy.check(&change);
            let cloaked = vis_disputes
                .iter()
                .any(|d| d.kind == Kind::Visibility && d.severity == Severity::High);
            let mut disputes = eng.diff_snapshots(&change.base, &change.head)?;
            disputes.extend(vis_disputes);

            let verdict = if cloaked {
                Verdict::Cloaked
            } else if visibility_policy.embargo_until.is_some() {
                Verdict::Embargoed
            } else {
                meaning_policy.evaluate(&disputes)
            };

            let docket = Docket {
                change: change.name.clone(),
                source: change.source.as_str().to_string(),
                base: change.base_ref.clone(),
                head: change.head_ref.clone(),
                disputes,
                scope: "auto".into(),
                authors: change.authors.clone(),
                verdict,
                embargo: visibility_policy.embargo_note(),
            };
            print!("{}", docket.render());
        }
    }
    Ok(())
}

fn load_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    files: &mut std::collections::HashMap<String, String>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            load_dir(root, &p, files)?;
        } else {
            let content = std::fs::read_to_string(&p)?;
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(rel, content);
        }
    }
    Ok(())
}
