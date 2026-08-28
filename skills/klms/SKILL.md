---
name: klms
description: Use the installed `klms` CLI for fast, read-only access to KAIST KLMS dashboards, courses, activities, grades, and attendance. Prefer it over browser scraping for supported reads; use kaist-cli only to create or refresh authentication.
---

# KLMS CLI

Use `klms --json` for machine-readable work. The global flag precedes the
command:

```bash
klms --json doctor
klms --json dashboard
klms --json courses list
klms --json courses show COURSE
klms --json activities list --course COURSE --week 3
klms --json grades show --course COURSE
klms --json attendance show --course COURSE
```

Prefer numeric course IDs after discovery; titles and codes are accepted only
when they resolve unambiguously. Read `ok`, the process exit status, and
`warnings`. JSON schema details live in the repository's `docs/JSON.md`.

The tool is intentionally read-only. Do not reinterpret these commands as
authorization to submit work, start quizzes, check into attendance, post
messages, or follow external activity links.

Authentication comes from a Playwright storage-state file. Inspect it without
printing secrets using `klms --json auth status`. If the result is
`AUTH_REQUIRED`, use the separate `kaist` CLI's established auth refresh flow;
`klms` does not own interactive login yet. Never display cookie values or ask
the user to paste a password into chat.
