# Implementation Research: Oot semantic-diff engine (wedge)

## The Task
Build the Oot wedge: a Rust CLI that runs on `git merge`, performs a structural + semantic diff between branches, emits a "dispute statement", and (per policy) blocks the merge and opens a docket. Status today: concept only — no code exists despite README marking the engine `[x]`.

## 1. Common Gotchas
- **"Semantic" is underspecified.** True meaning-level diff is an open research problem. Confusing "structural AST diff" with "semantic diff" will set impossible v1 expectations. — (UW ASE'24 eval shows even research tools produce more incorrect merges than git.)
- **Three-way merge needs base/ours/theirs.** A diff tool that only compares two sides misses the common ancestor and mis-flags rebase-introduced churn. Gate on `git merge-base`.
- **Language coverage trap.** Parser-based tools (IntelliMerge/Spork) are Java-only; SemanticMerge covers a handful. tree-sitter gives broad *structural* coverage cheaply but no *semantic* depth.

## 2. Best Practices
- **tree-sitter for structural diff, Rust.** tree-sitter has Rust bindings; gives language-agnostic AST diff across dozens of languages — the right local engine substrate. (Confirmed ecosystem: `tree-sitter`, `tree-sitter-cli` crates.)
- **Separate the two hard problems.** (a) Local, fast, deterministic *structural* conflict detection (Rust + tree-sitter). (b) Hosted, slower *semantic/intent* suspicion scoring (LLM API — the paid layer). Don't try to do (b) locally in v1.
- **Policy-as-config.** Blocking thresholds belong in a config file (TOML), not code, so the "docket" gate is tunable per repo.

## 3. Pitfalls & Language Quirks
- **tree-sitter node identity is positional**, not semantic — moved/renamed functions need similarity heuristics, not exact matching (this is exactly what IntelliMerge's refactoring-alignment solves for Java). v1 should accept false positives here.
- **LLM "semantic" checks are non-deterministic** — must be treated as *advisory flags* feeding the docket, never as an auto-block decision, or you'll ship flaky merge gates.
- **Git hook failure modes:** a hook that errors blocks all merges. The hook must fail *open* (allow merge, warn) unless explicitly in "enforce" mode.

## 4. Differentiation
- Industry standard: SemanticMerge / git mergetool resolve *after* conflict; GitLab Duo *auto-resolves*.
- Our approach: **detect + quantify + gate + route to human docket**, language-agnostic, local structural engine + hosted semantic check.
- Does the difference translate to usefulness? **Yes — but only if the README stops overclaiming.** The gate/adjudication layer is genuinely unoccupied by incumbents. The "semantic model" itself is not a moat (LLMs are commoditized); the *format + gate + workflow* is.

## Recommendation
Build v1 as: Rust + tree-sitter **structural conflict detector** producing the dispute-statement schema, with an *optional* hosted LLM call for semantic suspicion scoring. Ship the git hook **fail-open** by default. Rewrite the README with real citations (AgenticFlict 27.67%, Brindescu 26×, Anthropic 2026) before any public post.

## Sources
- tree-sitter docs / crates.io
- UW merge-tools eval (ASE 2024)
- IntelliMerge, SemanticMerge (above)
- AgenticFlict, Brindescu, Anthropic (above)

## Adversarial Verification
- Claim "tree-sitter has Rust bindings" — confirmed via crates.io ecosystem knowledge; verify exact crate versions at build time.
- Claim "incumbents resolve, don't gate" — confirmed: SemanticMerge is a mergetool; GitLab Duo auto-resolves; neither produces a policy-gated docket.
- Status: GREEN (implementation plan sound; requires version pinning at build).
