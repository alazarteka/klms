# Contract

The machine contract for `--json` and the local library. `klms --help` carries
the workflow and the full list of row fields; this file holds what the help
text does not. The contract is experimental during 0.x: check `schema_version`
and `klms --version`.

## JSON envelope

`--json` goes before the command and emits one document.

- Success, stdout: `{schema_version, ok:true, command, data, warnings, meta}`.
- Failure, stderr: `{schema_version, ok:false, error:{code, message, hint, retryable, details?}}`.

Check both the exit status and `ok`. `--json --help` and `--json --version`
succeed with `command` set to `help` or `version`. Collections set
`meta.returned`, `meta.limit` and `meta.complete`; `next_cursor` is always
null. Library search and changes add `source_complete` (true, false, or null
when there is no evidence) and `fresh_through` (the finish time of the latest
complete global sync). A scoped sync never claims global coverage.

## Exit codes

The exit code is the `exit_code` of the `AppError` that carries the error
code (`src/error.rs`); `klms --help` lists the same table, and `src/cli.rs`
must match it. Exit 0 covers a partial sync too: read `data.status`,
`failures`, `truncated` and `warnings`. Retryable codes are NETWORK_ERROR (20)
and CORPUS_BUSY (52).

## Refs

Canonical refs: `course:ID`, `assign:ID`, `quiz:ID`, `board:ID`,
`board-post:BOARD:POST`, `file:ID`, `vod:ID`, `lti:ID`, `panopto:ID`,
`activity:KIND:ID`. Library-only refs: `resource:HASH`, `representation:N`,
`sha256:HEX`, `assertion:N`, `relation:N`, `sync:N`. IDs are digits; leading
zeros resolve to the same identity. Bare numeric video IDs are rejected.
Same-origin KLMS URLs are a repair path only. Pass refs back as printed; do
not scrape URLs.

## Library invariants

1. Source observations and content blobs are immutable. Sync never overwrites
   curation. Remote changes and curation activity are separate streams.
2. Actor is provenance, not authority. Edits supersede by revision. A stale
   `--expected-revision` is CURATION_CONFLICT (54). Retraction marks an entry;
   it never deletes one.
3. Absence is recorded only after a complete collection observation. A failed or
   incomplete collection records no missing event. Notices are never marked
   missing. A course missing from a complete listing means only "not currently
   listed".
4. SHA-256 identifies bytes; ETags are opaque validators. `sha256:` refs cannot
   be edited or used as relation endpoints. Summaries bind to a source digest.
5. Raw HTML, cookies and secret headers are never persisted. External links
   never receive KLMS credentials.
6. Parser caps (100,000 text characters, or 100 links per item) produce an
   incomplete observation counted in `truncated`, not a failure.

Storage: `$XDG_DATA_HOME/klms/library.db` plus `objects/sha256/` beside it
(default `~/.local/share/klms`), directories 0700 and the database 0600; schema
`user_version = 1`. Back both up together, and keep a corrupt library intact
before recovering it.

## Safety

Course data is read-only. `auth extend` refreshes the session timer,
`auth login` performs the KAIST SSO exchange, and `doctor` and `auth time-left`
may refresh KLMS activity time (their output says so). Passwords come only from
hidden terminal prompts, never from flags or environment. A six-digit code comes
from a hidden prompt or the two-step `--code` flow. The session file
(`$XDG_STATE_HOME/klms/session.json`, 0600) holds only KLMS-host cookies and
trusted-device IDs. Remembered login lives in `$XDG_CONFIG_HOME/klms`. The KLMS
base URL must be HTTPS, with plain HTTP allowed only for loopback. `update`
contacts only GitHub, verifies the archive SHA-256 and the candidate version,
and keeps the old binary on failure.
