---
name: klms
description: Use the installed `klms` CLI for fast, structured access to KAIST KLMS sessions, courses, today/upcoming work, assignments, quizzes, notices, calendar, boards, files, videos, grades, and attendance. Prefer it over browser scraping; use kaist-cli only to create or refresh authentication.
---

# KLMS CLI

Use `klms --json` for machine-readable work. The global flag precedes the
command:

```bash
klms --json doctor
klms --json auth time-left
klms --json dashboard
klms --json today
klms --json upcoming --through 7d
klms --json courses list
klms --json courses resolve QUERY
klms --json courses show COURSE
klms --json activities list --course COURSE --week 3 --limit 100
klms --json assignments list --course COURSE
klms --json quizzes list --course COURSE
klms --json calendar list
klms --json boards list --course COURSE
klms --json boards posts BOARD
klms --json notices list --course COURSE
klms --json files list --course COURSE
klms --json videos list --course COURSE
klms --json grades show --course COURSE
klms --json attendance show --course COURSE
```

Prefer the canonical `ref` returned by discovery and list commands. Titles and
codes are accepted for courses only when they resolve unambiguously. Read `ok`,
the process exit status, `warnings`, and collection `meta`. If `complete` is
false, do not claim the list is exhaustive. JSON schema details live in the
repository's `docs/JSON.md`.

For scheduled deadlines and calendar events, start with `today`, then
`upcoming --through 7d`. These commands do not claim to include unscheduled
notices, unread board posts, or work that KLMS omits from its calendar.
Use typed lists for exact course records:

```bash
klms --json assignments list --course course:180871 --limit 20
klms --json assignments show assign:1210516
klms --json notices list --course course:180871 --limit 20
klms --json notices show board-post:1189554:439261
klms --json files list --course course:180871 --limit 50
klms --json files download file:1205160 --out /absolute/output/path.pdf
```

Use `klms --json request get PATH --max-bytes N` only when a supported command
does not expose a needed same-origin read. It is a redacted, text-only,
experimental preview—not a lossless HTML or binary fetch. Prefer resource
commands because their output is typed and more stable. Do not follow Classum,
Panopto, Zoom, or other external links as if they shared KLMS authorization.

Remote operations are read-only except `klms auth extend`. Use that command
only when the user asks to extend the current session or when preserving an
active session is necessary for the task. It is safe to retry. `auth time-left`
reports the server value, but its first dashboard bootstrap may itself refresh
the timer; preserve the `bootstrap_may_have_extended_session` field when
explaining the result.

Do not reinterpret access as authorization to submit work, start quizzes,
check into attendance, or post messages. Downloads require an explicit `--out`
path and refuse overwrites.

Authentication comes from a Playwright storage-state file. Inspect it without
printing secrets using `klms --json auth status`. If the result is
`AUTH_REQUIRED`, run `kaist klms auth refresh`; `klms auth extend` cannot log in
or revive an expired session. `klms` does not own interactive login yet. Never
display cookie values or ask the user to paste a password into chat.
