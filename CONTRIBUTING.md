# Contributing to Oot

Oot is the court for code. It judges **changes**, not commits or branches. A change is a delta between two snapshots and it can come from a human on git, an agent on Jujutsu, or a model in memory. The bridge goes both ways: git and Jujutsu are first class sources, and export gives you back byte-identical git. If you have been burned by repo-level permissions, by a secret that should never have been a file, or by a diff that went public too early and fucked you, this is your project.

## Who should jump in

- **VCS folks.** Oot reads snapshots from git and Jujutsu, keeps its own store, and exports back to git. Turning snapshots into Changes and stores back into git is real, unglamorous work and we need help.
- **Store and export nerds.** `.oot/` holds a bare git object db plus a change-id DAG. Export rewrites trees under policy but stays byte-faithful when unfiltered. If storage shit is your thing, come.
- **Policy folks.** Embargoed releases are still done by hand today, GitHub Security Advisories and the Git git-security list. People who have done this know where it leaks as hell.
- **Language folks.** Structural engine needs real heuristics for what counts as a conflict in each language.
- **Anyone who shipped the wrong merge.** Your war stories are our test cases. We love that shit.

## The model in one paragraph

A **Change** is a delta between two snapshots. It has an **Intent** (what it says it means), a **Visibility** policy (private-to, embargo-until, public), and authorship. Oot makes a **Docket**: the disputes it found, a **Verdict** (`adjudicated`, `blocked`, `embargoed`, `cloaked`), and embargo state. The engine is content addressed. It never needs a working tree on disk, so it runs fine in an agent's memory.

## Where the work lives

Repo is a seed, not a finished runtime. Build order is fixed because each bit feeds the next.

1. **Change ingestion.** Adapters that turn git and Jujutsu snapshots into Changes. This is the front door.
2. **Store and exporter. DONE.** `.oot/` keeps history natively: bare git odb, change-id DAG, `record`, `log`, `status`, `update`, and export that is byte-identical when unfiltered and strips private paths when filtered.
3. **Visibility policy.** The spine. Private paths, private branches, embargo schedules. Driven by a config file, not code. This is why Oot exists in the first place, the whole `.env` and monorepo privacy mess.
4. **Meaning disputes.** Structural engine (tree-sitter today) plus hosted intent check. Flags changes that agree on tokens but disagree on meaning. One axis, after visibility.
5. **Docket format.** On-disk record of a judgement, with visibility and embargo, so a human can review later.
6. **In-memory execution.** No materialized tree needed, for agents. This shit has to be fast.
7. **Hosted model client.** Intent scoring and embargo delivery. Only part that is not open source.

Original pitch was "Git settles lines, Oot settles meaning." Now governance leads, meaning follows, custody carries both.

## The docket contract

Every adjudication makes the same shape. Treat this as spec.

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

A dispute has four fields: `id`, `location`, `kind` (`meaning` or `visibility`), and `severity`. Severity feeds the policy threshold. If you add a `kind`, you must say how policy treats it. No magic.

## Dev setup

You need a recent Rust toolchain. Thats it.

```bash
cargo build --release
cargo test
```

Run it on this repo's fixtures to see a docket:

```bash
./target/release/oot adjudicate --change feature/auth-refactor \
  --base fixtures/repo/base --head fixtures/repo/head \
  --visibility fixtures/visibility.toml
# exits 1 on purpose: fixture touches secrets/.env so it cloaks
```

Walk the native store end to end:

```bash
./target/release/oot init
./target/release/oot status
GIT_AUTHOR_NAME=you GIT_AUTHOR_EMAIL=you@example.com ./target/release/oot record -m "my change"
./target/release/oot log
./target/release/oot update --dry-run
./target/release/oot export --out /tmp/exported   # add ./visibility.toml to filter private paths
```

## Conventions

- Keep the engine free of network calls. Hosted model is a separate client, keep that crap out.
- Engine takes byte blobs, not file paths. It must run with no working tree on disk.
- Tests are fixtures first. A change that makes the wrong docket is a test, not a bug report.
- Gate must never block a change it cant reason about. When in doubt, adjudicate and log.
- Plain output. Docket is read by humans in a terminal, keep it narrow and scannable. No fancy bullshit.

## License

Adjudication runtime, docket format, adapters, store and exporter are MIT. Hosted model is a paid service and not in this repo. Contributions to open parts land under MIT. Simple.

## Start here

Open an issue with the merge or disclosure that still pisses you off. Tell us what the tools got wrong. That chat is the real spec, not some doc.
