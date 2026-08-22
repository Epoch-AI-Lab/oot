# Known friction

## ~~Fixture `.env` policy noise~~ RESOLVED 2026-08-21

Originally `VisibilityPolicy::check` flagged private-path fragments against
*every* file in the head snapshot, so the intentional `.env` fixture cloaked
every change. Fixed by aligning the code with its own documented contract:
only paths *touched* by a change (added, removed, or content-modified vs
base) are checked. See `test_visibility_policy_only_flags_touched_private_paths`.

## ~~Binary change detection is lossy~~ RESOLVED 2026-08-22

`Snapshot.files` now stores raw bytes (`HashMap<String, Vec<u8>>`). Change
detection compares bytes exactly, so two distinct binaries never compare
equal even when their lossy text collapses to the same U+FFFD sequence.
Text conversion happens only in the structural engine at parse time
(`as_text`, src/engine/mod.rs). All three ingestion paths (dir, git, jj)
store unconverted bytes. Pinned by
`test_cli_distinct_binaries_are_not_collapsed`.

## ~~Same-named functions tracked by first occurrence only~~ RESOLVED 2026-08-22

`FunctionMap` was `HashMap<name, Def>`, so a second same-key definition in
one file was dropped with an ambiguity note and the first occurrence's
changes shadowed the rest. The map now holds every definition
(`HashMap<String, Vec<FnDef>>`) and diffing aligns each name group by
content: exact body match first, positional pairing of leftovers, remainder
becomes added/removed. Qualified keys (`(Type).name`) stay rejected — they
are unstable under impl-block refactors
(`test_engine_impl_move_is_not_a_conflict`). Pinned by
`test_engine_duplicate_function_names_tracked_separately`,
`test_engine_go_same_name_methods_tracked_separately`, and
`test_engine_rust_impl_method_collision_tracked_separately`.

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

