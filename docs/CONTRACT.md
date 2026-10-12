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

## Course discovery

`courses list` and `courses resolve` default to the selected dashboard scope.
`--all` explicitly discovers all enrolled courses; `--year YEAR --semester
spring|summer|fall|winter` filters that discovery by course metadata. These reads
never change dashboard preferences. `meta.complete` requires exhaustion of the
upstream pagination as well as no output truncation. Unknown terms remain null
and are excluded by term filters. `term` retains the `YEAR-N` representation,
where course-code suffixes 1/2/3/4 mean spring/summer/fall/winter; dashboard
selector values 10/11/20/21 are a different encoding. Media paths are not term
evidence. Printed historical `course:ID` refs work with all course resources.
Library sync retains its dashboard default and never crawls history implicitly.

## Exit codes

The exit code is the `exit_code` of the `AppError` that carries the error
code (`src/error.rs`); `klms --help` lists the same table, and `src/cli.rs`
must match it. Exit 0 covers a partial sync too: read `data.status`,
`failures`, `truncated` and `warnings`. Retryable codes are NETWORK_ERROR (20)
and CORPUS_BUSY (52).

Exit 10 carries three error codes: AUTH_REQUIRED (no session, or KAIST rejected
the sign-in), CODE_INCORRECT (`auth login --code` with a wrong code; the pending
login is consumed) and CODE_EXPIRED (no pending login, or it or its code
expired; run `auth login` again). Branch on `error.code`, not the exit status.

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

## Videos (JSON schema 5)

Schema 5 replaces the former activity-shaped video rows; the local SQLite schema
remains version 1. Video grammar is independent of assignment/quiz grammar. `videos list --course`
returns recordings and collections with `video_type`; verified Panopto
collections expand into recording rows. A failed launch or incomplete folder
page makes `meta.complete` false and produces a warning. LTI recognition uses
launch protocol markup, never its title. `downloadable` and
`official_download_available` are null when delivery sources have not been
inspected. The official DownloadUrl permission is separate from readable
playback MP4/HLS availability.

`panopto-recording:KIND:CM:UUID` identifies an actual recording and preserves its
KLMS launch activity for renewed authentication. KIND is `lti`, `panopto` or
`panoptocourseembed`; CM is numeric. Existing `vod:ID`, `lti:ID`, `panopto:ID`
and `panoptocourseembed:ID` identify Moodle activities. Collections cannot be
downloaded as a single video; choose a recording ref.

`videos download REF --out PATH` publishes a private file without overwriting.
It defaults to a 4 GiB media cap and 1800-second download/remux budget;
`--max-bytes` permits up to 16 GiB and `--max-seconds` up to 7200. Native VOD
uses only static same-origin `/vod1/` MP4 sources. Panopto exchanges use separate
host-scoped transient credentials; media requests receive neither KLMS nor
Panopto credentials. Temporary signed media URLs are never returned in JSON;
Panopto `source_url` is the KLMS launch locator. HLS media is retrieved under
bounded transport policies and remuxed locally with ffmpeg; ffmpeg receives
only local paths. No player scripts or playback/progress telemetry run.
Multiple native Panopto streams are preserved as separate video/audio tracks
with their start offsets; a readable composed podcast takes precedence.
`stream_count` and `output_composition` expose this choice on inspected recordings.
Finite HLS playlists, external audio renditions, initialization fragments and
byte ranges are supported. Encrypted or unfinished/live playlists fail visibly.
Library sync remains metadata-only for videos, even with `--files`.
