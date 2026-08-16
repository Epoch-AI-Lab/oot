# Contributing to Oot

Oot is the court for code. It adjudicates **changes**, not commits or branches. A change is a content-addressed delta between two snapshots, and it can arrive from a human on git, an agent on Jujutsu, or a model in memory. If you have been burned by repository-level permissions, by a secret that should never have been a file, or by a diff that went public too early, this is your project.

## Who should contribute

- **VCS adapter authors.** Oot reads snapshots from git and Jujutsu but owns neither. Turning those snapshots into Changes is real, unglamorous work.
- **Policy people.** Embargoed releases are run by hand today (GitHub Security Advisories, the Git project's git-security list). The people who have done this know where it leaks.
- **Language experts.** The structural engine needs heuristics for what counts as a real conflict in each language.
- **Anyone who has shipped the wrong merge.** Your war stories become our test cases.

## The model in one paragraph

A **Change** is a delta between two snapshots. It carries an **Intent** (what it claims to mean), a **Visibility** policy (private-to, embargo-until, public), and authorship. Oot produces a **Docket**: the disputes it found, a **Verdict** (`adjudicated`, `blocked`, `embargoed`, `cloaked`), and any embargo state. The engine is content-addressed. It never assumes a materialized working tree, so it runs inside an agent's memory.

## Where the work lives

The repo is a seed, not the finished runtime. The build order is fixed because each piece feeds the next.

1. **Change ingestion.** Adapters that turn git and Jujutsu snapshots into the Change type. This is the front door.
2. **Visibility policy.** The governance spine. Private paths, private branches, embargo schedules. Driven by a config file, not code. This leads because the `.env`, monorepo-privacy, and private-branch problems are the reason Oot exists.
3. **Meaning disputes.** The structural engine (tree-sitter today) plus a hosted intent check. Flags changes that agree on tokens but disagree on meaning. One axis, after visibility.
4. **Docket format.** The on-disk record of an adjudication, including visibility and embargo state, so a human can review later.
5. **In-memory execution.** The path that runs with no materialized tree, for agents.
6. **Hosted model client.** The intent scoring and embargo distribution. The only part that is not open source.

The original one-line pitch was "Git settles lines, Oot settles meaning." That framing is now one axis of three. Governance (visibility and embargo) leads; meaning follows.

## The docket contract

Every adjudication produces the same shape. Treat this as the spec.

```
change:     <name>
from:       <source: git | jj | memory>
base:       <ref>
head:       <ref>
meaning:    <n> disputes detected
visibility: <private paths>, <embargoed until date>
scope:      <areas touched>
authors:    <list>

dispute-01: <what and where>     [meaning | visibility]

verdict:    ADJUDICATED | BLOCKED | EMBARGOED | CLOAKED
embargo:    <held for maintainers until date>
```

A dispute has four required fields: `id`, `location`, `kind` (`meaning` or `visibility`), and `severity`. `severity` feeds the policy threshold. If you add a `kind`, you must also say how policy treats it.

## Dev setup

You need a recent Rust toolchain. Once the runtime exists:

```bash
cargo build --release
cargo test
```

Run the runtime against a fixture change to see a docket:

```bash
./target/release/oot adjudicate --change feature/auth-refactor
```

## Conventions

- Keep the engine free of network calls. The hosted model is a separate client.
- The engine takes byte blobs, not file paths. It must run with no working tree on disk.
- Tests are fixtures first. A change that produces the wrong docket is a test, not a bug report.
- The gate must never block a change it cannot reason about. When in doubt, adjudicate and log.
- Plain output. The docket is read by humans in a terminal, so keep it narrow and scannable.

## License

The adjudication runtime, docket format, and adapters are MIT licensed. The hosted model is a paid service and is not part of this repo. Contributions to the open parts land under MIT.

## Start here

Open an issue with the merge or the disclosure that still bothers you. Tell us what the tools got wrong. That conversation is the real spec.
