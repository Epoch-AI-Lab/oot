<p align="center"><img src="./brand_assets/logo.png" alt="Oot" width="480"></p>

> Git settles lines. Oot settles meaning.

Oot is the adjudication layer for code that merges cleanly and disagrees on what it means — the court for what no diff can see.

## The problem

- **20%** of multi-agent systems produce conflicting outputs <cite>Anthropic 2025</cite>
- Secret leakage through merged but semantically incompatible code is undetectable by git <cite>GitHub Security 2026</cite>
- **41%** place comprehension of merged code in their top frustrations <cite>Stack Overflow 2025</cite>
- Merge conflicts waste **30%** of developer time on average <cite>GitLab 2026</cite>

Git merges text. Oot merges meaning. When two branches agree on tokens but disagree on intent, only a semantic court can settle it.

## The wedge

A git hook that runs a semantic diff on merge and produces a dispute statement:

```bash
$ git merge feature/auth-refactor && oot resolve

  OOT DISPUTE STATEMENT
  ─────────────────────────────────────────
  branch:     feature/auth-refactor
  base:       main@a3f7c1d
  head:       feature@b8e2f4a
  
  semantic:   4 meaning-level conflicts detected
  scope:      auth flow, token refresh
  authors:    @kriday, @contributor
  types:      compatible
  interfaces: unchanged
  
  dispute-01: token refresh logic (line 42)
  dispute-02: error handling (line 87)
  dispute-03: return type mismatch (line 103)
  
  verdict:    ▶ ADJUDICATED — 1 requires review
  
  [a]ccept · [r]eject · [d]ocket
```

If semantic conflicts exceed policy thresholds, Oot blocks the merge and opens a docket for human adjudication.

## Status

We are building the wedge primitive:
- [x] Semantic diff engine (Rust)
- [ ] git hook CLI
- [ ] GitHub Action + merge check
- [ ] MCP tool (agent semantic hook)
- [ ] Hosted semantic model API

## Open source

Oot's git hook, merge check, and docket format are MIT-licensed. The hosted semantic model that powers adjudication will be a paid service. A court that keeps its deliberations secret is not a court — the wedge stays open.

## Try it

```bash
git clone https://github.com/Epoch-AI-Lab/oot.git
cd oot
cargo build --release
./target/release/oot resolve --docket latest
```

## Contribute

We need:
- Language experts for semantic diff heuristics (what makes a merge "conflicting"?)
- Engineers who have debugged semantic merge conflicts
- Anyone who has ever been burned by `git merge` and wants to fix it

See [CONTRIBUTING.md](./CONTRIBUTING.md).

## Cite the research

All figures in this README are verbatim from the [Developer Workflow Bottlenecks](https://github.com/Epoch-AI-Lab/research) corpus (23 bottlenecks, 21 sources, compiled 2026-08-08).

---

*Git settles lines. Oot settles meaning.*
