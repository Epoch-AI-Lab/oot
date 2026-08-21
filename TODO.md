# Known friction

## ~~Fixture `.env` policy noise~~ RESOLVED 2026-08-21

Originally `VisibilityPolicy::check` flagged private-path fragments against
*every* file in the head snapshot, so the intentional `.env` fixture cloaked
every change. Fixed by aligning the code with its own documented contract:
only paths *touched* by a change (added, removed, or content-modified vs
base) are checked. See `test_visibility_policy_only_flags_touched_private_paths`.

No open items.
