# Known friction

## ~~Fixture `.env` policy noise~~ RESOLVED 2026-08-21

Only paths *touched* by a change are checked against private-path policy now.
Pinned by `test_visibility_policy_only_flags_touched_private_paths`.

## ~~Binary change detection is lossy~~ RESOLVED 2026-08-22

`Snapshot.files` stores raw bytes; text conversion happens only at parse time.
Pinned by `test_cli_distinct_binaries_are_not_collapsed`.

## ~~Same-named functions tracked by first occurrence only~~ RESOLVED 2026-08-22

`FunctionMap` holds every same-named definition and diffing aligns each name
group by content. Pinned by `test_engine_duplicate_function_names_tracked_separately`
and friends.

## Rename/rename divergence is swallowed

Base has `f`; ours renames `f -> g`, theirs renames `f -> k`. Both sides
deleted `f`, so it reads as convergent; `g` and `k` read as plain additions.
Merged result silently holds both names. Pre-existing (the old code hit its
convergent-clean branch the same way), but def-level tracking makes a fix
reachable: detect that the removed base defs survive under different names
per side, then emit High. Trigger: first real rename/rename dispute or the
hosted intent-scoring work, whichever comes first.

## Positional pairing can swap row attribution on count asymmetry

When a new same-named def lands *above* a modified one
(base 1x `f`, head 2x `f`), leftover pairing reports "changed" at the new
def's row and "added" at the modified one's row. Counts and severity are
right; only line numbers are swapped. Right fix: similarity-based leftover
pairing (edit distance or tree-sitter diff hash) instead of document order.
Trigger: when a docket consumer starts using dispute rows for navigation.

## Fallback conflict message says "modified" for pure deletion divergence

When both branches delete different copies of a same-named def, the High
dispute reuses the "both branches modified function `X` differently"
wording (severity is correct, only the verb is off). Right fix: a dedicated
"deleted differently" message variant. Trigger: first docket consumer that
pattern-matches dispute details.

## ~~Signed commits lose their signatures~~ RESOLVED 2026-08-22

Dogfooding on this repo found it immediately: 4 GitHub-signed merge commits
diverged on export, rewriting every downstream SHA. Fix: export now reuses the
original commit object from the store odb whenever every parent exported to
its own original sha (`source_sha` on `ChangeRecord` + identity fast path in
`replay`). Signatures only break downstream of genuinely rebuilt changes —
exactly where step 2's visibility filtering will rebuild anyway.
Pinned by `tests/store_roundtrip_test.rs::test_roundtrip_preserves_extra_commit_headers`.

# In progress: Oot store + git exporter (dogfooding Oot on Oot)

Decision (2026-08-22): Oot becomes future source control. Bridge to GitHub is
an exporter, not a fork of git. Storage is a bare git odb inside `.oot/`
(jj-style: model is ours, bytes are git's).

## DONE — all tests green (`cargo test`: 11 suites, incl. round-trip)

- `src/store.rs` — `.oot/` store: bare git odb at `.oot/objects.git`,
  `changes/<id>.json` records content-addressed via `git hash-object`,
  `.oot/map/<orig-sha>` for idempotent import, `.oot/refs/<branch>`,
  append-only `.oot/.index` in import order.
- Parents are stored as **change ids** (not original commit shas) so the DAG
  lives in Oot's own address space.
- CLI: `oot init`, `oot import --repo <src>` (all branches), `oot export --out <dir>`.
- Export attaches the store odb via `<out>/.git/objects/info/alternates`.
  Because trees/authors/timestamps/offsets/messages/parents are preserved,
  git reproduces **byte-identical commit SHAs** — pinned by
  `tests/store_roundtrip_test.rs::test_full_roundtrip_preserves_every_commit_sha`
  (merge commit, binary blob, unicode message, non-UTC offsets).
- Gotchas already hit (do not rediscover): root commits have empty `%P`;
  `git log --pretty=format:` inserts `\n` between entries (records use `\x01`);
  `commit-tree`/`hash-object` skip writing objects that already exist via
  alternates, which is why update-ref needed the alternates *file*, not just env.

## NEXT (in order)

1. ~~Dogfood for real~~ DONE 2026-08-22.
2. ~~Visibility-filtered export~~ DONE 2026-08-22: `oot export` auto-loads
   `./visibility.toml` (or `--visibility <path>`); changes touching private
   paths are withheld, kept trees are rebuilt minus those paths via recursive
   ls-tree/mktree rewriting, children remap to nearest kept ancestors, empty
   results skipped, embargoed stores refuse export entirely. Every decision
   lands in `.oot/export-log.jsonl`. Export cache is policy-scoped: switching
   policies wipes `.oot/export/` mappings. Pinned by
   `tests/export_visibility_test.rs` (strip, embargo, empty-skip, cache).
   Verified on this repo: faithful export byte-exact; filtered export strips
   `fixtures/repo/head/secrets/.env` and logs both touching commits.
   Mirror pushes use an explicit empty policy to stay byte-faithful.
3. ~~`oot record`~~ DONE 2026-08-22: `oot record -m <msg> [--branch <name>]`
   snapshots the working copy into the store odb (blobs + recursive mktree,
   exec bits kept), creates a native change (`source_sha: null`) as child of
   the branch head, refuses no-op records. Identity from GIT_AUTHOR_* /
   GIT_COMMITTER_* envs then git config. Ignore rules: git check-ignore in
   git worktrees; minimal root-.gitignore matcher (names, dirs, `*`) for
   pure-Oot projects. Export already handles native changes via the
   reconstruction path — pinned end-to-end by `tests/record_test.rs`,
   including mixed imported+native history.
4. ~~`oot log` / `oot status`~~ DONE 2026-08-22: status diffs the working
   copy against the branch head's tree (content-addressed, no odb pollution);
   log walks reachable changes newest-first with `[git]`/`[oot]` provenance
   tags and offset-aware dates (pure arithmetic, no date deps). Both share
   `resolve_branch` with record. Pinned by `tests/log_status_test.rs`.

## Deliberate cuts (v1)

- `record` ignores nested .gitignore files outside the root, negation
  (`!pattern`) rules, and `?`/`[]` glob classes. Git worktrees get full
  semantics via git itself; only pure-Oot projects see the subset.

- Tags and annotated tags are not imported/exported.
- Signed commits downstream of a rebuilt change lose their signatures
  (reconstruction cannot forge them; SHAs then differ). Unmodified history is
  byte-exact — see resolved friction above.
- Any filtered export rebuilds ALL commits (identity fast path off), so even
  clean signed commits lose signatures when one taint exists. Per-subtree
  reuse is possible later if it matters.
- Filtered exports still share the store odb via alternates: withheld blobs
  are unreachable from refs (a push transfers nothing) but resolve locally
  until `git repack -a -d` runs in the export. Never hand out a filtered
  export directory alongside its `.oot`.
- Commit messages must be valid UTF-8 without `\x00`/`\x01`; violations fail loudly.
- Exported repo reads blobs through alternates; run `git repack -a -d` in the
  export once before deleting `.oot` if you want it standalone.
- No gc/pruning in the store yet.

---

# Night run (2026-08-22, Kriday pre-approved everything below - zero questions mode)

Positioning line APPROVED: "Oot holds history in order to govern it."
Small calls (naming, defaults, wording) are delegated; document each call in
the PR body. Branches stack where noted. Do NOT merge PRs.

## PR-O1 `fix/rename-rename-dispute` (bases on main, engine files only)

- Swallow point: the 3-way convergent check fires vacuously on empty
  surviving lists (`src/engine/mod.rs` ~line 300).
- Per file, in the all-three-exist branch, stash side-tagged pools BEFORE any
  case dispatch: `removed[side] = (name, base_idx, FnDef)` from `gone`,
  `added[side] = (name, FnDef)` from `fresh`. This changes no existing
  emissions.
- `find_divergent_renames(removed_ours, added_ours, removed_theirs,
  added_theirs)`: greedy pair removed-to-added per side on EXACT signature
  equality (body with own name blanked) AND new_name != old_name,
  consumed-once on both sides; cross-side join on `(base_name, base_idx)`;
  fire only when both sides paired AND names differ.
- Emit ONE High dispute per divergently renamed base def. Detail:
  "3-way conflict: both branches renamed function `f` differently (`f` ->
  `g` in target, `f` -> `k` in incoming)". Location: theirs row of k,
  fallback ours row, then 0. No docket schema change.
- Suppress claimed theirs-fresh defs from `pending_added` so k is not also
  reported as a Low addition. Ours-side g needs no suppression.
- Convergent rename (same new name both sides) stays silent. Rename-vs-delete
  stays silent: pin as documented-gap test.
- Leave a seam (e.g. `rename_score(a, b)` returning an Option) for future
  similarity work but DO NOT switch off exact matching.
- Tests: pure rename/rename gives exactly one High with pinned detail string;
  delete/delete + coincidental adds unchanged; convergent rename clean;
  base {f,h} with ours f->g h->h2 and theirs f->k h->h2 gives exactly two
  Highs; rename + body edit on one side pinned as known gap; helper unit
  tests for multiplicity/consumed-once/equal-name rejection. Full suite green
  including adapters and CLI tests.

## PR-O2 `docs/positioning` (bases on main, docs only)

Approved positioning seed paragraph (adapt, keep honest both directions):
"Oot is the court for code: it governs changes - who may see them, what they
mean, and when they may ship. Governance needs custody, so since August 2026
Oot keeps its own history: a native `.oot/` store whose unit is the change,
with record, log, status, and visibility-filtered export built on it. Git
remains the universal interchange format - every store exports back to
byte-identical git history, and GitHub is reached through that exporter, not
through forking git. Today the store is an implementation detail serving the
court; the declared direction is that it becomes the source control itself."

- README.md:
  - Intro gains the custody story; REPLACE the "does not try to replace Git /
    never owns the repository" sentences (most false lines in the repo).
  - "Where Oot sits" bullet 1 replaced: Oot keeps its own store; git is the
    export target; unfiltered export reproduces byte-identical SHAs.
  - "How Oot thinks" gains a Store bullet (native history, parents as change ids).
  - Status checklist adds shipped items: native store (.oot/ odb, change-id DAG),
    import (idempotent via sha map), record (native changes), log/status
    (provenance tags), export (byte-identical roundtrip incl merge/binary/
    unicode/non-UTC offsets, signed headers kept downstream of unrebuilt
    changes), visibility-filtered export (withhold, strip trees, remap,
    embargo refuses, decisions logged). One honest-limits footnote pointing
    at TODO deliberate cuts.
  - Try-it gains the native workflow block (init/status/record/log/export)
    and labels the showcase invocation as illustrative or swaps runnable
    flags (current mock invocation exits 2).
  - License section enumerates the store. "Someday" becomes "Roadmap":
    move hosted intent scoring + embargo distribution down, add working-copy
    update, store-to-court adjudication, per-subtree signature reuse, gc.
  - Discipline rule: present tense ONLY for test-pinned facts; everything
    else gets today/next/planned markers. Never close an enumeration of
    sources with "only".
- CONTRIBUTING.md: adapter framing becomes two-way bridge (git/jj first-class
  sources, exporter keeps git interchange); add "Store & exporter
  contributors" audience; build order gains storage/exporter marked DONE;
  dev setup gains five-command store walkthrough; license list adds store.
- oot-vision-report.html: add SUPERSEDED banner at top ("Superseded
  2026-08-22: Oot now keeps its own store; see TODO.md decision"). No rewrite.
- visibility.toml comments document dual consumers: adjudicate treats touched
  private paths as High disputes (cloak + exit 1); export withholds touching
  changes, rebuilds trees minus those paths, embargo makes export refuse.
- Cargo.toml description / clap about may shift to match positioning.

## PR-O3 `feat/store-court` (bases on main; store.rs + new src/court.rs + main.rs)

- NEVER mutate ChangeRecord / content addressing. Governance lives in sidecars.
- `.oot/dockets/<change-id>.json`: latest docket. Schema: `{schema: 1,
  change, tree, parents, adjudicated_at, policy_key, docket}` (docket is the
  existing Docket struct verbatim). Overwrite on re-run.
- `.oot/adjudications.jsonl`: append-only audit, one line per run:
  `{epoch, event:"adjudicated", change, verdict, meaning:N, visibility:M,
  policy_key}` (mirrors export-log.jsonl style).
- `policy_key`: deterministic hash/fingerprint of MeaningPolicy +
  VisibilityPolicy (same trick as the export cache key).
- New Store methods: `read_blob`, snapshot-from-tree (ls-tree -r -z then
  cat-file per blob), `resolve_change(id-or-prefix)` failing loudly with
  candidate list on ambiguous prefixes.
- `src/court.rs`: head = change's tree; base = FIRST parent tree (root change
  = empty snapshot); authors from record; run engine + visibility +
  finalize_adjudication. Merge changes diff against first parent in v1.
  Provenance tag `[oot]` vs `[git]` by `source_sha`.
- CLI: `oot adjudicate --change <id|prefix>` engages ONLY when a store opens,
  the id resolves, and none of --base/--head/--base-ref/--head-ref/--source/
  --repo/--docket were given (existing modes untouched). Persist by default,
  `--no-save` opt-out. Exit codes unchanged (0 only Adjudicated).
- `oot docket <id|prefix>` renders the persisted docket.
- Export coupling stays loose: dockets reference ids, the DAG never references
  dockets. No export gating in this PR.
- `oot record` prints a hint line "next: oot adjudicate --change <short>".
- Tests (`tests/adjudicate_store_test.rs`): happy root change exit 0;
  meaning dispute + block_on review means BLOCKED exit 1; child change shows
  delta not whole-tree noise; persistence + overwrite + exactly one jsonl
  line per run; imported mid-history change carries [git] tag; mixed
  imported+native history; unknown id / ambiguous prefix loud errors; unit
  tests for read_blob/snapshot_from_tree/save-load/policy_key.

## PR-O4 `feat/oot-update` (STACKED on PR-O3 branch)

- PHASE 0 REQUIRED FIRST: `Store::tree_entries(tree)` keeping FULL modes
  (100644, 100755, symlink 120000, gitlink 160000); keep existing
  `tree_files()` as wrapper so status/export call sites are untouched.
  Reject non-UTF-8 paths loudly. (Today modes are silently dropped - that
  corrupts materialized trees.)
- `.oot/HEAD`: one line, `ref: <branch>` or bare change id (detached).
  `init` writes `ref: main`. Missing HEAD file = legacy resolve_branch
  behavior so existing stores/tests pass. Once present, record/status/log
  default through HEAD (fixes multi-branch papercut).
- Command: `oot update [--branch <name>] [--change <id>] [--dry-run]
  [--force]`. No args materializes current branch head (legacy fallback when
  no HEAD). `--change ID` = detached materialize.
- Dirty semantics: REFUSE (no stash in v1) when tracked paths are
  modified/deleted vs the source tree OR an untracked file would be
  overwritten with different content. Factor status's delta computation into
  shared `worktree_deltas(store, root, tree)` so status and update cannot
  drift. Untracked-but-untouched files are fine. Error lists offending paths,
  suggests record-first or --force.
- Fast path: same tree = "up to date", bookkeeping only. Per-file: compare
  blob_sha against on-disk file, rewrite only mismatches (converges, avoids
  mtime churn).
- Apply order: write-new-content, apply-deletions, flip HEAD LAST. An
  interrupted update is repaired by rerunning (idempotent crash recovery).
- Materialization: single `git cat-file --batch` process; temp+rename atomic
  writes; exec bits restored; deletions come ONLY from old-tree minus
  new-tree path set (never filesystem scans); prune now-empty parent dirs
  best-effort; symlinks materialized with std::os::unix::fs::symlink
  refusing absolute or ..-escaping targets (record still skips them -
  document asymmetry); gitlinks skipped with warning; empty dirs
  unrecoverable, documented.
- Path safety BEFORE any disk write: reject absolute paths, `..` components,
  `.git`/`.oot`/`.jj` components, case-insensitive-FS collisions. All git
  plumbing with explicit --git-dir.
- `--dry-run` prints planned A/M/D + ref moves, touches nothing. `--force`
  overrides refusals with printed data-loss warning. Update never consults
  .gitignore and never gates on visibility.toml (store-local operation).
- Tests (`tests/update_test.rs`): restore roundtrip bytes + exec bits;
  up-to-date no-op; branch switch removes stray tracked file, keeps
  unrelated untracked; dirty refusal nonzero + paths listed + disk
  unchanged; --force proceeds; untracked collision refused; empty dir
  pruned; dry-run zero mutations; HEAD bookkeeping incl. record defaulting
  to HEAD branch in multi-branch store (previously error); detached
  semantics (status works, record refuses); imported repo wipe-then-update
  byte-equal (binary + unicode name + merge); malicious ../escape crafted
  tree fixture fails loudly writing nothing outside root; half-applied state
  converges. Unit tests: tree_entries modes/kinds, path-safety validator,
  deltas. Audit log_status_test.rs expectations against HEAD changes.

## Night-run status

- [x] PR-O1 fix/rename-rename-dispute
- [ ] PR-O2 docs/positioning
- [ ] PR-O3 feat/store-court
- [ ] PR-O4 feat/oot-update

Mark your PR's box `[x]` in the same branch before opening it.
