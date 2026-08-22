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
3. `oot record` — capture working-copy deltas as new changes without git
   (first true "Oot as source of control" write path).

## Deliberate cuts (v1)

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
