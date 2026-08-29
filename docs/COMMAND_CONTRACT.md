# Command contract

`klms` is designed for both people and agents. Its grammar follows
`klms RESOURCE VERB [OPERAND] [OPTIONS]`, with a few top-level jobs such as
`dashboard` and `doctor` where another noun would add noise.

## Experimental 0.x surface

The human grammar is intended to evolve additively. The machine contract is
still experimental while the typed coursework model is completed; consumers
should check `schema_version` and the installed binary version. We will make
schema corrections deliberately and document them rather than preserving a
misleading field forever.

```text
klms doctor
klms auth status
klms auth time-left
klms auth extend
klms dashboard [--limit N]
klms today [--course COURSE] [--limit N]
klms upcoming [--through Nd] [--course COURSE] [--limit N]
klms courses list [--limit N]
klms courses resolve QUERY [--limit N]
klms courses show COURSE
klms activities list --course COURSE [--week N] [--kind KIND] [--limit N]
klms assignments list --course COURSE [--limit N]
klms assignments show ASSIGNMENT
klms quizzes list --course COURSE [--limit N]
klms quizzes show QUIZ
klms calendar list [--limit N]
klms boards list --course COURSE [--limit N]
klms boards posts BOARD [--limit N]
klms boards show BOARD_POST_REF
klms notices list --course COURSE [--limit N]
klms notices show NOTICE
klms files list --course COURSE [--limit N]
klms files download FILE_REF_OR_URL --out PATH
klms videos list --course COURSE [--limit N]
klms videos show VIDEO
klms grades show --course COURSE
klms attendance show --course COURSE
klms request get PATH [--max-bytes N]
```

`COURSE` accepts a `course:ID` reference, numeric course id, exact course code
or title, or an unambiguous title/code fragment. List commands return canonical
references for follow-up commands. Assignments, quizzes, notices, files, and
video kinds accept those references; same-origin KLMS URLs remain a repair
path. Bare numeric video ids are rejected because they do not identify whether
the module is VOD, Panopto, or LTI. List commands have finite defaults and hard
maxima.

## Output

Human-readable output is the default. `--json` emits exactly one success
document on stdout or one error document on stderr. Diagnostics and progress
never share stdout with structured data. The envelope is versioned separately
from the binary.

Successful resource reads include stable ids, canonical `ref` values, and URLs
whenever KLMS exposes them. Typed assignment and quiz lists include normalized
ISO 8601 deadlines with the Korea-time offset while preserving KLMS's display
text. Empty recognized lists are successful results. Unrecognized upstream
markup is an explicit shape error. Ambiguous course resolution names the
candidates; it never guesses.

`today` and `upcoming` are human-oriented calendar views. They use Asia/Seoul
calendar dates, preserve exact timestamps, and compose the same typed calendar
records returned to agents. `--through 7d` includes today through seven days
ahead and is bounded to 90 days.

## Safety

All remote operations are reads except `auth extend`, which explicitly asks
KLMS to refresh the current session timer. It is safe to retry and reports the
server-authoritative remaining duration after the touch.

Downloads require `--out`, create a new file atomically, and refuse to replace
an existing path even when another process creates it during the download.
`request get` is an experimental repair hatch for known same-origin HTML and
JSON read routes. It uses GET, rejects action-like paths and secret-bearing
queries, enforces the normal redirect policy, and emits a bounded preview.
HTML is reduced to visible text and common token assignments are redacted.
Complete JSON is structurally redacted; incomplete bounded JSON is omitted
rather than risking disclosure. Other content types are rejected. Prefer a
typed command whenever one exists.

`doctor` validates the live session with a dashboard GET and reports whether a
failure is missing configuration, expired authentication, network reachability,
or another error. Because KLMS may count authenticated page reads as activity,
`doctor` and a cold `auth time-left` disclose that their bootstrap request may
refresh the session timer.

The CLI does not submit assignments, begin quiz attempts, post to boards,
check into attendance, or operate third-party services.

## Help and specification

The Clap declaration in `src/cli.rs` is the single executable source of truth.
Every resource and verb has examples in layered `--help`; contract tests
exercise the grammar and JSON protocol. We deliberately do not maintain a
second hand-written Usage/KDL command tree that can drift from the executable
one. A generated Usage specification can be added when a real downstream
consumer needs it.
