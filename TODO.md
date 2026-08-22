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

