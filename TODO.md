# Known friction

## ~~Fixture `.env` policy noise~~ RESOLVED 2026-08-21

Originally `VisibilityPolicy::check` flagged private-path fragments against
*every* file in the head snapshot, so the intentional `.env` fixture cloaked
every change. Fixed by aligning the code with its own documented contract:
only paths *touched* by a change (added, removed, or content-modified vs
base) are checked. See `test_visibility_policy_only_flags_touched_private_paths`.

## Binary change detection is lossy

Snapshots store file contents as `String`, so non-UTF8 files go through
`String::from_utf8_lossy`, which collapses every invalid byte sequence to
U+FFFD. Two *different* binaries can therefore compare equal and register as
unchanged — a governance docket could then say "no files changed" for a
change that did alter a binary. This affects both `load_dir`
(src/main.rs) and the git adapter's `extract_snapshot` (src/adapter/git.rs).

**Trigger:** when binary artifacts (images, lockfiles, compiled blobs)
become first-class inputs to adjudication, or any real docket shows a
false "no files changed".

**Right fix:** store raw bytes (or a content hash) in `Snapshot.files` and
convert to text only at tree-sitter parse time, so touched-path detection
compares bytes exactly.

Pinned by the lossy-conversion note in `load_dir` until then.

