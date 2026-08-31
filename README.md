<p align="center"><img src="./brand_assets/logo.png" alt="Oot" width="480"></p>

> Repos track lines. Oot governs changes: who may see them, what they mean, and when they may ship.

Oot is a court for code. Not another git clone. It judges your **changes**, who can see them, what they mean, when they ship.

To judge shit properly it needs custody. So since August 2026 Oot keeps its own history in `.oot/`. The unit is the change, not the commit. `record`, `log`, `status`, `update`, and `export` all run off it. Git is still the interchange, every store exports to byte-identical git history and GitHub is reached through that export, not a fork of git. A change can come from a human on git, an agent on Jujutsu, or a model in memory. Oot judges them the same. It started as a five minute sketch about semantic merge conflicts. Now its about governance, and meaning is just one axis.

## Why this exists

Agents write a lot of our merges now and its kind of a mess. A study of 142k agent PRs found 27.67% hit conflicts, and the nasty ones touched 500 lines across files (AgenticFlict, [arXiv:2604.03551](https://arxiv.org/html/2604.03551)). When the conflict is about meaning not text, it ships bugs. Code from semantic conflicts is 26x more likely to be buggy, that shit is scary (Brindescu et al., [Empirical Software Engineering](https://stairs.ics.uci.edu/papers/2020/emperical_MC.pdf)). Anthropic threw three agents on one repo with different goals, they started a turf war and merged clean but fought each other ([multiagent systems](https://www.anthropic.com/research/multiagent-systems)).

Git's shape is plain wrong for this. Permissions are per repo, so keeping one file private means a secret manager and a prayer ([git-crypt](https://github.com/AGWA/git-crypt) does crypto, not policy). Branches and PRs are heavy as hell, [Jujutsu](https://github.com/jj-vcs/jj) already showed we dont need that crap. And a real working tree is slow as hell: cloning thousands of tiny files takes 30 to 40s on macOS APFS, 3 to 12s on Linux.

Git and Jujutsu track lines. Oot holds history so it can govern it.

## How Oot thinks: changes, not commits

One idea. A **Change** is a delta between two snapshots. You can judge it without a checkout, without a working tree. Everything else follows.

- **Change**. A delta between two snapshots, from anywhere.
- **Store**. Native history in `.oot/`: a bare git object db plus change records. Parents are change ids, not commit shas. `record`, `log`, `status`, `update`, `export` live here.
- **Visibility**. The spine. Policy on paths or branches: `private-to`, `embargo-until`, `public`. This fixes `.env`, monorepo privacy, and private branches as policy, not crypto.
- **Intent**. What the change says it means. We check semantic disputes against this.
- **Dispute**. A disagreement. Either visibility (you touched a private path) or meaning (two changes clash on intent).
- **Docket**. The record: disputes, verdict, visibility, embargo.
- **Verdict**. `adjudicated`, `blocked`, `embargoed`, or `cloaked`.

## How it works

A change arrives. Oot checks it and prints a docket. Real run from this repo's fixtures:

```bash
$ ./target/release/oot adjudicate --change feature/auth-refactor \
    --base fixtures/repo/base --head fixtures/repo/head \
    --visibility fixtures/visibility.toml

  OOT DOCKET
  ─────────────────────────────────────────
  change:     feature/auth-refactor
  from:       git
  base:       fixtures/repo/base
  head:       fixtures/repo/head

  meaning:    1 disputes detected
  visibility: 1 private path(s)

  intent:     secrets/.env, src/lib.rs
  authors:    @you

  dispute-01: both sides changed `login` (src/lib.rs:1)    [meaning]
  dispute-02: private path secrets/.env touched by @you (secrets/.env)    [visibility]

  verdict:    ▶ CLOAKED . 1 requires review, cloaked
  embargo:    patch held for maintainers until 2026-09-01

  [a]ccept · [r]eject · [d]ocket
```

It exits 1 because it touched `secrets/.env`. Exit 0 is `adjudicated`, everything else is nonzero so CI can gate on it.

If it breaks policy, we block or cloak the hell out of it. If its a security fix, we can embargo it until a date you set, same way Git and GitHub do it manually ([OSSF guide](https://github.com/ossf/oss-vulnerability-guide), [Git embargo](https://www.kernel.org/pub/software/scm/git/docs/howto/coordinate-embargoed-releases.html)). Detection and blocking ship today. Quietly sending held patches to maintainers is still todo, that part is hard.

## Where Oot sits

- **Custody, then interchange.** Oot keeps history in `.oot/`. Git is the export target. Unfiltered export is byte for byte identical (round-trip tests prove it). Adapters still read snapshots straight from git and Jujutsu.
- **No checkout needed.** The engine takes byte blobs and works on the parse tree. It runs in an agent's memory isolate. An agent in a worktree cant hold `main` hostage.
- **Crypto is not our job.** Real encryption is git-crypt or a hosted key service. Oot owns the policy and the gate, not the math.

## Status

Working seed and a bit rough around the edges. Engine runs, docket renders, git and Jujutsu ingestion are in memory, store ships with import, record, log, status, update, export. We use Oot to govern Oot's own damn changes.

- [x] Change ingestion from git snapshots (in-memory via `git ls-tree`/`cat-file`) and materialized dirs
- [x] Jujutsu ingestion (in-memory via `jj file list`/`file show`, revsets, real conflict detection)
- [x] Visibility policy: private paths, private branches, embargo schedules
- [x] Meaning disputes from the structural engine (tree-sitter: Rust, Go, JavaScript, TypeScript, TSX, Python)
- [x] Docket format with visibility and embargo (JSON/TOML + render)
- [x] In-memory path (no checkout needed for git)
- [x] Git adapter with 3-way adjudication
- [x] Jujutsu adapter with 3-way adjudication (`--source jj`, revsets)
- [x] Native store: `.oot/` holds bare git odb plus change-id DAG
- [x] Import from git: all branches, idempotent via sha map
- [x] `oot record`: capture working copy as native change, refuses no-ops
- [x] `oot log` / `oot status`: `[git]`/`[oot]` tags, offset-aware dates
- [x] `oot update`: put any stored change back on disk, 3-way merge that keeps dirty work, `--force` to discard, `--dry-run` to preview, `.ootkeep` keeps placeholder dirs
- [x] `oot adjudicate --change` + `oot docket`: judge stored changes directly, sidecar dockets + audit log
- [x] `oot gc` / `oot prune`: sweep unreferenced changes, dockets, mappings, and pack/prune the bare Git ODB with grace periods
- [x] Export to git: byte-identical round-trip (merges, binaries, unicode, non-UTC), sigs survive when not rebuilt
- [x] Visibility-filtered export: withhold private changes, rebuild kept trees minus those paths, skip empties, embargo blocks export, GPG signatures survive on untouched history prefixes, log to `.oot/export-log.jsonl`

Same-named defs in one file (two `render` methods) are tracked separately: we match identical bodies first, then pair the rest, so only the real change is reported.

Cuts for now: nested ignore negation in pure-Oot projects, tags, sigs downstream of rebuilt history, and a few more.

## License

Adjudication runtime, docket format, adapters, store, garbage collector, and exporter are MIT. A court that hides its logic is not a fucking court, so the gate stays open.

## Roadmap

We havent built this shit yet. It needs real users to be worth the cost, and we have none so we aint rushing.

- **Hosted intent scoring.** A model that checks what a change says vs what it does. Structural engine catches *that* code changed, this catches *what it means*. Needs a server and a model and someone to pay.
- **Embargo distribution.** Actually sending held patches quietly to maintainers before the public diff. Needs keys and auth. Detection already ships.
- **Per-subtree signature reuse on rewritten commits.** Rebuilding with selective signature reuse.

## Try it

```bash
git clone https://github.com/Epoch-AI-Lab/oot.git
cd oot
cargo build --release

# adjudicate a fixture change (exits 1: touches secrets/.env)
./target/release/oot adjudicate --change feature/auth-refactor --base fixtures/repo/base --head fixtures/repo/head --visibility fixtures/visibility.toml

# or 3-way git (in-memory, no checkout)
./target/release/oot adjudicate --change feature/auth-refactor --base-ref main --head-ref feature/auth --repo .

# or 3-way jujutsu (revsets fine)
./target/release/oot adjudicate --source jj --change greet --base-ref 'bookmarks(exact:main)' --head-ref '@-'

# or skip snapshots: keep history natively in .oot/
./target/release/oot init
./target/release/oot status
GIT_AUTHOR_NAME=you GIT_AUTHOR_EMAIL=you@example.com ./target/release/oot record -m "first change"
./target/release/oot log
./target/release/oot update --dry-run
./target/release/oot update --change a1b2c3d
./target/release/oot gc --dry-run
./target/release/oot gc --force
./target/release/oot export --out exported   # auto-applies ./visibility.toml when present
```

## Contribute

We need folks who have been burned by repo-level permissions, leaked diffs, and clean merges that ship bugs. If that shit pissed you off, talk to us:

- **VCS folks** who know git and Jujutsu guts and can turn snapshots into Changes
- **Policy folks** who have done embargoed releases and know where they leak
- **Language folks** for the structural engine
- **Anyone** who thinks a clean merge that ships a bug is worse than a blocked one

See [CONTRIBUTING.md](./CONTRIBUTING.md).

---

*Repos track lines. Oot governs changes.*
