# Known friction

## Fixture `.env` policy noise

`visibility.toml` flags any path containing `.env`, so every branch touching
`fixtures/repo/head/secrets/.env` gets a CLOAKED verdict + exit 1 — even though
that file is an intentional test fixture (tracked on purpose, see `.gitignore`
exception).

Correct behavior today, but it will fire on nearly every fixtures-touching
branch and train us to ignore exit codes. When it starts feeling like noise,
the fix is policy scoping — e.g. an ignore/exempt list in VisibilityPolicy for
`fixtures/` paths — not weakening the rule.

First flagged: 2026-08-21, during the first dogfood run of oot on itself.
