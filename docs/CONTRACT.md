# Contract

Facts that `klms --help` does not carry. The machine contract is experimental
during 0.x: check `schema_version` and `klms --version`.

## JSON

`--json` (before the command) emits one document: success on stdout
`{schema_version, ok:true, command, data, warnings, meta}`, failure on stderr
`{schema_version, ok:false, error:{code, message, hint, retryable, details?}}`.
Check both exit status and `ok`. `--json --help` and `--json --version` succeed
with `command` `help`/`version`. Collections set `meta.returned/limit/complete`;
`next_cursor` is always null. Library search/changes add `source_complete`
(latest global sync: true, false, or null = no evidence) and `fresh_through`
(finish of the latest complete global sync); a scoped sync never claims global
coverage. Secrets, cookies and raw SSO responses never appear in output.
Exit codes and error codes are listed in `klms --help`.

## Refs

Canonical refs: `course:ID`, `assign:ID`, `quiz:ID`, `board:ID`,
`board-post:BOARD:POST`, `file:ID`, `vod|lti|panopto:ID`, `activity:KIND:ID`;
library-only: `resource:HASH`, `representation:N`, `sha256:HEX`,
`assertion:N`, `relation:N`, `sync:N`. IDs are digits; leading zeros resolve to
the same identity. Bare numeric video ids are rejected. Same-origin KLMS URLs
are a repair path only.

## Library invariants

1. Source observations and content blobs are immutable; sync never overwrites
   curation. Remote changes and curation activity are separate streams.
2. Actor is provenance, not authority. Edits supersede by revision; a stale
   `--expected-revision` is CURATION_CONFLICT. Retraction marks, never deletes.
3. Absence is recorded only after a complete collection observation. A failed or
   incomplete collection records no missing event. Notices are never marked
   missing; a missing course means only "not currently listed".
4. SHA-256 identifies bytes (ETags are opaque validators); `sha256:` refs cannot
   be edited or used as relation endpoints. Summaries bind to a source digest.
5. Raw HTML, cookies and secret headers are never persisted. External links
   never receive KLMS credentials.
6. Parser caps (100,000 text characters or 100 links) yield incomplete
   observations counted in `truncated`, not failures.

Storage: `$XDG_DATA_HOME/klms/library.db` plus `objects/sha256/` beside it
(default `~/.local/share/klms`), modes 0700/0600; schema `user_version = 1`.
Back both up together; keep a corrupt library before recovering. Library-specific
errors: MIGRATION_REQUIRED, CORPUS_BUSY, CURATION_CONFLICT, CONTENT_UNAVAILABLE
(candidates in `error.details.representations`), CORPUS_CORRUPT, LIBRARY_IO.

## Safety

Course data is read-only; `auth extend` refreshes the session timer, `auth login`
performs the KAIST SSO exchange, and `doctor`/`auth time-left` may refresh KLMS
activity time (they say so). Passwords and codes come only from hidden terminal
prompts or the two-step `--code` flow, never flags or environment. The session
file (`$XDG_STATE_HOME/klms/session.json`, 0600) holds only KLMS-host cookies and
trusted-device ids; remembered login lives in `$XDG_CONFIG_HOME/klms`. Non-loopback
service URLs must be HTTPS. `update` contacts only GitHub, verifies the archive
SHA-256 and candidate version, never downgrades, and keeps the old binary on
failure.
