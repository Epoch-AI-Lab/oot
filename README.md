<p align="center"><img src="./brand_assets/logo.png" alt="Oot" width="480"></p>

> Repos track lines. Oot governs changes: who may see them, what they mean, and when they may ship.

Oot is the court for code. It governs your **changes**: who may see them, what they mean, and when they may ship. Governance needs custody, so since August 2026 Oot keeps its own history in a native `.oot/` store whose unit is the change, with `record`, `log`, `status`, and visibility-filtered export built on it. Git remains the universal interchange format. Every store exports back to byte-identical git history, and GitHub is reached through that exporter rather than through a fork of git. Today the store serves the court; the declared direction is for it to become the source control itself. A change can still arrive from a human on git, an agent on Jujutsu, or a model running in memory, and Oot judges all of them the same way. The project began as a five minute sketch about semantic merge conflicts. The real target is wider: governance over changes, with meaning as one axis among three.

## Why this exists

Agents now write a large share of our merges. A 2026 study of 142,652 AI-agent pull requests found that **27.67% hit merge conflicts**, and the bad ones touched around 500 lines across several files (AgenticFlict, [arXiv:2604.03551](https://arxiv.org/html/2604.03551)). When a conflict is about meaning rather than text, it ships bugs: a 2020 study of 143 open-source projects found code from semantic merge conflicts is **26 times more likely to be buggy** (Brindescu et al., [Empirical Software Engineering](https://stairs.ics.uci.edu/papers/2020/emperical_MC.pdf)). Anthropic's 2026 red-team study put three agents on one repo with conflicting goals; they slid into a turf war and produced code that merged cleanly but fought each other (Anthropic, [multiagent systems](https://www.anthropic.com/research/multiagent-systems)).

The deeper problem is that Git's primitives are the wrong shape for this world. Permissions are repository-level, so keeping one file private means a third-party secret manager and a prayer ([git-crypt](https://github.com/AGWA/git-crypt) exists, but it does encryption, not policy). Branches and pull requests add overhead that tools like [Jujutsu](https://github.com/jj-vcs/jj) have already shown we do not need. And a materialized working tree is a bottleneck: cloning or reinstalling thousands of small files takes 30 to 40 seconds on macOS APFS where Linux does it in 3 to 12.

Git and Jujutsu track lines. Oot holds history in order to govern it, and it answers the questions they were never built to answer.

## How Oot thinks: changes, not commits

Oot's unit is the **Change**, a content-addressed delta between two snapshots. Judging one needs no checkout and no working tree. From that one idea the rest follows.

- **Change**. A delta between two snapshots, from anywhere.
- **Store**. Native history in `.oot/`: a bare git object database plus change records whose parents are change ids rather than commit shas. `record`, `log`, `status`, and `export` build on it.
- **Visibility**. The governance spine. A policy on paths or branches: `private-to`, `embargo-until`, `public`. This is the `.env`, monorepo-privacy, and private-branch problem, handled as policy rather than cryptography.
- **Intent**. What the change claims to mean. Semantic disputes are checked against this. Meaning is one axis, not the whole product.
- **Dispute**. A point of disagreement. Either *visibility* (a policy is violated) or *meaning* (two changes diverge in intent).
- **Docket**. The adjudication record: disputes, verdict, visibility state, embargo date.
- **Verdict**. `adjudicated`, `blocked`, `embargoed`, or `cloaked`.

## How it works

A change arrives. Oot runs its checks and prints a docket. Real run against this repo's fixtures:

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

This run exits 1 because the change touched `secrets/.env`, a private path, so it gets cloaked. Exit code 0 means the verdict was `adjudicated`; every other verdict is nonzero, so CI and agent loops can gate on it.

If a dispute crosses policy, Oot blocks the change or cloaks the private parts. If the change is a security fix, Oot can hold it under embargo until a date you set in the policy, the way the Git project and GitHub Security Advisories already do manually ([OSSF guide](https://github.com/ossf/oss-vulnerability-guide), [Git embargo process](https://www.kernel.org/pub/software/scm/git/docs/howto/coordinate-embargoed-releases.html)). Detection and enforcement ship today; quietly distributing held patches to maintainers is still on the roadmap.

## Where Oot sits

- **Custody, then interchange.** Oot keeps its own history in `.oot/`, a native store whose unit is the change. Git is the export target: an unfiltered export reproduces every original commit SHA, byte for byte (pinned by the round-trip tests), while adapters still read snapshots straight from git and Jujutsu.
- **Execution is content-addressed.** The engine takes byte blobs and works on the parse tree. It never assumes a checked-out working tree, so it runs inside an agent's memory isolate and an agent in a worktree cannot hold `main` hostage.
- **Cryptography is delegated.** Actual encryption goes to git-crypt or a hosted key service. Oot owns the policy and the gate, not the math.

## Status

Working seed. The engine runs, the docket renders, git and Jujutsu ingestion are in-memory, and the native store ships with import, record, log, status, and export. Current focus: using Oot to govern Oot's own changes.

- [x] Change ingestion from git snapshots (in-memory via `git ls-tree`/`cat-file`) and materialized dirs
- [x] Jujutsu ingestion (in-memory via `jj file list`/`file show`, revset resolution, first-class conflict detection)
- [x] Visibility policy: private paths, private branches, embargo schedules (the governance spine)
- [x] Meaning disputes from the structural engine (tree-sitter: Rust, Go, JavaScript)
- [x] Docket format with visibility and embargo state (JSON/TOML + render)
- [x] In-memory execution path (no materialized tree required for git)
- [x] Git adapter with 3-way adjudication
- [x] Jujutsu adapter with 3-way adjudication (`--source jj`, revsets accepted)
- [x] Native store: `.oot/` holds a bare git object database plus a change-id DAG (parents stored as change ids, not commit shas)
- [x] Import from git: all branches, idempotent via a sha map
- [x] `oot record`: captures the working copy as a native change, refuses no-op records
- [x] `oot log` / `oot status`: `[git]`/`[oot]` provenance tags, offset-aware dates
- [x] Export to git: byte-identical round-trip, including merge commits, binaries, unicode messages, and non-UTC offsets; signatures survive downstream of unrebuilt changes
- [x] Visibility-filtered export: changes touching private paths are withheld, kept trees rebuilt minus those paths, empty results skipped, embargoed stores refuse export entirely, every decision logged to `.oot/export-log.jsonl`

Same-named definitions in one file (e.g. a `render` method on two classes) are each tracked separately: diffing matches identical bodies first, then pairs the rest, so only the definition that actually changed is reported.

Honest limits live in [TODO.md](./TODO.md) under "Deliberate cuts": nested ignore rules and negation patterns in pure-Oot projects, tags, signatures downstream of rebuilt history, gc, and more.

## License

The adjudication runtime, the docket format, the adapters, and the store with its exporter are MIT licensed. A court that hides its deliberations is not a court, so the gate stays open.

## Roadmap

Deliberately unbuilt. These need users to be worth their cost, and there are none yet.

- **Working-copy update.** Materialize a stored change back onto disk, `oot update` style. Planned next; today you read history with `log` and export to git to check anything out.
- **Store-to-court adjudication.** Run the engine straight off stored changes instead of snapshots. Planned.
- **Hosted intent scoring.** A model that checks what a change claims to mean against what it actually does. The structural engine catches *that* code changed; this would catch *what it means*. Needs a server, a model, and someone paying for both.
- **Embargo distribution.** The courier half of embargo: quietly shipping held patches to maintainers before the public diff drops. Needs keyed private channels and maintainer auth. The detection half already ships.
- **Per-subtree signature reuse.** Any filtered export currently rebuilds every commit, so even clean signed commits lose signatures. Reusing unchanged subtrees would keep more of them.
- **Store gc and pruning.** The store grows without bounds today.

## Try it

```bash
git clone https://github.com/Epoch-AI-Lab/oot.git
cd oot
cargo build --release

# adjudicate a fixture change (exits 1: the fixture touches secrets/.env)
./target/release/oot adjudicate --change feature/auth-refactor --base fixtures/repo/base --head fixtures/repo/head --visibility fixtures/visibility.toml

# or 3-way git (in-memory, no checkout)
./target/release/oot adjudicate --change feature/auth-refactor --base-ref main --head-ref feature/auth --repo .

# or 3-way jujutsu (revsets welcome)
./target/release/oot adjudicate --source jj --change greet --base-ref 'bookmarks(exact:main)' --head-ref '@-'

# or skip snapshots entirely: keep history natively in .oot/
./target/release/oot init
./target/release/oot status
GIT_AUTHOR_NAME=you GIT_AUTHOR_EMAIL=you@example.com ./target/release/oot record -m "first change"
./target/release/oot log
./target/release/oot export --out exported   # auto-applies ./visibility.toml when present
```

## Contribute

We need people who have been burned by repository-level permissions, leaked diffs, and clean merges that ship bugs:

- **VCS adapter authors** who know git and Jujutsu internals and can turn snapshots into Changes.
- **Policy people** who have run embargoed releases and know where the process leaks.
- **Language experts** for the structural engine's semantic heuristics.
- **Anyone** who thinks a clean merge that ships a bug, or a public diff that burns a zero-day, is the worse failure.

See [CONTRIBUTING.md](./CONTRIBUTING.md).

---

*Repos track lines. Oot governs changes.*
