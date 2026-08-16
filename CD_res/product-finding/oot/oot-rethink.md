# Oot rethink: commits, branches, permissions, in-memory

## Source material
User supplied notes (transcribed from a talk, appears to be Theo / t3.gg) arguing Git's primitives are broken:
- Rethink commits (okay but not great)
- Rethink branches and PRs
- Granular / file-level permissions: keep files private in a repo, private branches / private in-flight PRs in an OSS repo, embargoed security patches to maintainers before public diff, private sub-packages in monorepos
- JJ / Jujutsu: snapshots + tags instead of commits/branches; less cognitive overhead
- Git worktrees are painful, especially for AI agents (agent in a worktree checked out main and held it hostage)
- In-memory / node isolates: source control shouldn't need an OS filesystem; APFS clean-install 30-40s vs Linux 3-12s; tools like just-bash run code in memory

## Grounding (verified)

### JJ / snapshots
- JJ (jj-vcs.dev) uses a commit graph but hides it: working copy is auto-snapshotted as a commit, no index, no "current branch" (bookmarks instead), conflicts are first-class objects. It is Git-compatible (stores commits in a real Git repo). Source: jj-vcs docs, git-comparison.
- Conclusion: the "snapshot not commit" model is a solved, mature, Git-compatible product. Not Oot's fight.

### File-level permissions / secrets
- git-crypt: transparent per-file encryption via .gitattributes + gpg; devs without the key can still clone/commit. Limitation: only file *content* is encrypted, not filenames/metadata, and it's key-management heavy (revoking a user requires re-encrypting). Source: AGWA/git-crypt, git-secret.
- git-secret: gpg-based, similar.
- These solve *encryption*, not *policy/visibility adjudication*. None of them express "this file is private to these users," "this branch is embargoed until date X," or "merge this patch to maintainers quietly." That policy layer is unsolved and tool-agnostic.

### Embargoed / coordinated patches
- Real, established practice: GitHub Security Advisories + draft advisories + temporary private forks; OSSF maintainer guide; the git project itself coordinates embargoed releases via git-security list + distros@openwall. Source: GitHub Blog, OSSF guide, kernel.org git howto.
- But it is locked inside GitHub's (or a foundation's) walled garden. A tool-agnostic "quiet merge to maintainers, public later" gate does not exist as a standalone layer. This is exactly an adjudication/gate function.

### In-memory / filesystem bottlenecks
- The APFS-vs-Linux file-creation gap is a real, documented macOS pain (thousands of small files). Theo's "just-bash" runs a bash-like layer in JS memory.
- JJ's own model already operates "in memory" (step 2 of every command builds new commits in memory before touching the working copy). Source: jj-vcs working-copy docs.
- Conclusion: the lesson for Oot is a *design constraint*, not a new product. The engine should run on byte blobs / ASTs, not a materialized working tree, so it can execute inside an agent's memory isolate.

## Critical assessment: what fits Oot, what doesn't

### Fits (strengthens the thesis)
1. **Granular permissions + secrets + private branches + embargoed patches.** This extends "adjudication" from *meaning* to *visibility/access*. The "court" metaphor scales cleanly: Oot already emits a dispute statement and gates merges on policy. Adding a "visibility statement" (who may see/merge what, what is embargoed until when) is the same machinery pointed at a second axis. This is unsolved and tool-agnostic. High value, coherent.

### Design principle, not a product
2. **In-memory / content-addressed engine.** Adopt as a constraint now: the engine takes byte sources and produces ASTs; it never assumes a checked-out working tree. Cheap to honor in the current Rust engine (we already pass `&str` sources). This lets Oot run inside agent isolates and sidesteps the APFS trap. Do not build a filesystem.

### Reject as Oot's scope (use JJ instead)
3. **Rebuilding commits/branches as snapshots.** JJ owns this and is Git-compatible. If Oot rebuilds VCS primitives it abandons its actual wedge (the adjudication layer) and enters a fight it cannot win against a mature tool. Oot should be VCS-agnostic and sit on top of git *or* jj.

## Recommended narrowed thesis
Oot is the adjudication layer for code across two axes:
- **Meaning:** it settles merges that agree on tokens but disagree on intent (semantic disputes).
- **Visibility:** it settles who may see and merge what, and when a patch may go public (access/embargo disputes).

It is a policy gate, not a VCS and not a crypto layer. Crypto can delegate to git-crypt or a hosted KMS; the VCS can be git or jj; Oot's job is the court on top.

## Risk of the expanded scope
Permissions/visibility is a large surface (identity, access control, crypto, embargo scheduling). To stay YAGNI, v1 of the visibility axis = a "visibility statement" produced beside the dispute statement, driven by a policy file declaring private paths, private branches, and embargo dates; Oot blocks/cloaks accordingly. Actual encryption delegates to git-crypt or a hosted KMS. Oot owns policy + adjudication only.

## Sources
- Jujutsu docs: https://docs.jj-vcs.dev/latest/git-comparison/ , working-copy, bookmarks, glossary
- git-crypt: https://github.com/AGWA/git-crypt , git-secret: https://git-secret.io/
- GitHub Security Advisories / CVD: https://github.blog/security/vulnerability-research/a-maintainers-guide-to-vulnerability-disclosure-github-tools-to-make-it-simple/
- OSSF vulnerability guide: https://github.com/ossf/oss-vulnerability-guide
- git embargo process: https://www.kernel.org/pub/software/scm/git/docs/howto/coordinate-embargoed-releases.html

## Adversarial verification
- JJ model verified against jj-vcs docs (snapshot working copy, bookmarks not branches, Git-compatible). Confirmed real, mature.
- git-crypt verified (transparent per-file encryption, content-only, key-management heavy). Confirmed it does NOT do policy/visibility.
- Embargo practice verified (GitHub Advisories, OSSF, git kernel list). Confirmed it is platform-walled, not a standalone layer.
- In-memory claim: APFS gap is plausible/contingent on Theo's benchmarks (not independently re-run); JJ "in memory" step verified. Treated as design constraint, not a fact Oot depends on.
- Logical coherence: extending "court" to two axes (meaning + visibility) is coherent and does not require rebuilding VCS. Status: GREEN.
