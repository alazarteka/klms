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
`complete` is about the rows that passed the filters: a `today`/`upcoming`
`--kind` that matches no row in the date range adds a warning naming that kind
and the kinds that were present, so a misspelled kind is not mistaken for
"nothing due".

## Course discovery

`courses list` and `courses resolve` default to the selected dashboard scope.
`--all` explicitly discovers all enrolled courses; `--year YEAR --semester
spring|summer|fall|winter` filters that discovery by course metadata. These reads
never change dashboard preferences. `meta.complete` requires exhaustion of the
upstream pagination as well as no output truncation. Unknown terms remain null
and are excluded by term filters, with a warning counting them (`--all` shows
them). `term` retains the `YEAR-N` representation,
where course-code suffixes 1/2/3/4 mean spring/summer/fall/winter; dashboard
selector values 10/11/20/21 are a different encoding. Media paths are not term
evidence. Printed historical `course:ID` refs work with all course resources.
Library sync retains its dashboard default and never crawls history implicitly.

Change: `term` is now derived only from a trailing `_YYYY_N` shortname suffix
with N in 1-4. A code such as `training_2024_20` previously yielded a term and
now yields `null`.

## Exit codes

The exit code is the `exit_code` of the `AppError` that carries the error
code (`src/error.rs`); `klms --help` lists the same table, and `src/cli.rs`
must match it. Exit 0 covers a partial sync too: read `data.status`,
`failures`, `truncated` and `warnings`. Retryable codes are NETWORK_ERROR (20)
and CORPUS_BUSY (52).

Exit 10 carries three error codes: AUTH_REQUIRED (no session, or KAIST rejected
the sign-in), CODE_INCORRECT (a wrong code, from an interactive `auth login` or
`auth login --code`; with `--code` the pending login is consumed) and
CODE_EXPIRED (the code expired, or for `--code` no pending login is waiting or it
expired; run `auth login` again). Branch on `error.code`, not the exit status.
A NETWORK_ERROR during `auth login --code` keeps the pending login, so the same
code can be retried. `auth forget` purges every account in klms's dedicated
keyring service namespace across available backends, independently of login.json,
and removes login.json, the plaintext credentials file and the pending login.
It warns if the identity is unreadable or any deletion cannot be confirmed;
retrying forget after unlocking/restoring a backend works even without login.json.
`password_removed` is true only if a password was removed and password cleanup
was complete; `password_backends` reports each attempted backend separately
(true: removed, false: absent, null: failed or undetermined).
`password_cleanup_complete` is false when a backend is unreachable or cleanup
fails. Missing keyring helpers with no remembered backend also produce a warning.
`auth logout` removes the session and pending login, keeping remembered credentials.
Both commands remove crash-leftover `<owned filename>.<pid>.tmp` files for the
files they own, and report cleanup failures. Login cleanup failures are warnings
(in `error.details.warnings` if the login itself fails). Password writes are
rolled back if saving login.json fails; rollback failures are reported and can
be retried with forget.

## Refs

Canonical refs: `course:ID`, `assign:ID`, `quiz:ID`, `board:ID`,
`board-post:BOARD:POST`, `file:ID`, `vod:ID`, `lti:ID`, `panopto:ID`,
`activity:KIND:ID`. Library-only refs: `resource:HASH`, `representation:N`,
`sha256:HEX`, `assertion:N`, `relation:N`, `sync:N`. IDs are digits; leading
zeros resolve to the same identity. Bare numeric video IDs are rejected.
Same-origin KLMS URLs are a repair path only. Pass refs back as printed; do
not scrape URLs. Typed detail URLs accept only their identity parameters and are
rebuilt from the reference. `files download` accepts pluginfile locators and
resource, coursefile, or folder references. `request get` allows dashboard,
course, calendar, known module view/index and board-article endpoints, the user
grade report, attendance index, native video viewer, and pluginfile paths only;
endpoint-specific query keys are allowlisted, with no action or secret keys.

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
5. Parsed pages are not stored as HTML. Downloaded attachments, including HTML
   files, are stored verbatim. Cookies and secret headers are not stored in the
   library. External links never receive KLMS credentials.
6. Parser caps (100,000 text characters, or 100 links per item) produce an
   incomplete observation counted in `truncated`; the sync status is `incomplete`.

Storage: `$XDG_DATA_HOME/klms/library.db` plus `objects/sha256/` beside it
(default `~/.local/share/klms`), directories 0700 and the database 0600; schema
`user_version = 2` (version 2 only marks the dropped `sync_runs.failures` and
`representations.observed_*` columns; a 1-to-2 migration changes no data and
older libraries keep those unused columns, so an older binary refuses a newer
library with `MIGRATION_REQUIRED`; a newer schema than supported does too). Back both up together, and keep a corrupt library intact
before recovering it.

## Safety

Commands do not submit coursework or edit course data, and each command is
restricted to its own endpoints before transport, including redirects. Opening
activity pages (`show` commands and library sync detail fetches) lets KLMS record
views and completion; quiz views may also process expired attempts.
`auth extend` refreshes the session timer,
`auth login` performs the KAIST SSO exchange, and `doctor` and `auth time-left`
may refresh KLMS activity time (their output says so). Passwords come only from
hidden terminal prompts, never from flags or environment. A six-digit code comes
from a hidden prompt or the two-step `--code` flow. The session file
(`$XDG_STATE_HOME/klms/session.json`, 0600) holds only KLMS-host cookies and
trusted-device IDs. Remembered login lives in `$XDG_CONFIG_HOME/klms`. The KLMS
base URL must be HTTPS, with plain HTTP allowed only for loopback. `update`
contacts only GitHub, verifies the archive SHA-256 and the candidate version,
and keeps the old binary on failure. A legacy installed skill (the `klms skill`
command and SKILL.md are deprecated; `klms --help` is the manual) is never
removed: `update` lists its paths in `warnings` so the user can delete them.

Proxies are chosen per request as curl does: `https_proxy`/`HTTPS_PROXY` for
HTTPS and `http_proxy`/`HTTP_PROXY` for HTTP, then `all_proxy`/`ALL_PROXY`
(lowercase wins). `no_proxy`/`NO_PROXY` takes `*` or comma-separated hosts,
each also covering its subdomains (`kaist.ac.kr` covers `klms.kaist.ac.kr`; a
leading dot and IPv6 brackets are optional). Ports and CIDR ranges in
`NO_PROXY` are not supported. Loopback targets are never proxied.

Link lists in detail and dashboard output show opaque `mailto:` and `tel:`
links as written (fragment removed), and redact credentials and sensitive query
values in hierarchical non-HTTP links. They never fetch these links, and the
library does not record them. Other opaque schemes, `javascript:` and `data:`
links are omitted. Hosts use standard URL normalization, including IDN encoding.

## Videos (JSON schema 5)

Schema 5 replaces the former activity-shaped video rows; the local SQLite schema
is version 2. Video grammar is independent of assignment/quiz grammar. `videos list --course`
is metadata-only: it reads the course page and never opens an activity, so it
makes no `mod/*/view.php`, LTI launch or Panopto request. Its rows have
`video_type` `recording` (native VOD), `panopto` (a Panopto activity: one
recording or a collection) or `lti` (an external tool that may be Panopto), and
`downloadable` null. `videos show` and `videos download` open the activity in
KLMS and, for Panopto, perform its LTI launch and read Panopto delivery
metadata; Moodle logs that as an activity view (and may mark view completion),
and Panopto may create an account on first launch. `videos show` on a collection
lists recording refs, titles and durations. Failed folder enumeration is an
error; only a successfully returned but incomplete enumeration sets `complete`
false and produces a warning. If launch or playback-source inspection fails,
`videos show` still returns readable activity metadata with a warning. LTI
recognition uses launch protocol markup, never its title. `downloadable` and `official_download_available` are null when
delivery sources have not been inspected, including on collections. The
official DownloadUrl permission is separate from readable playback MP4/HLS
availability.

`panopto-recording:KIND:CM:UUID` identifies an actual recording and preserves its
KLMS launch activity for renewed authentication. KIND is `lti`, `panopto` or
`panoptocourseembed`; CM is numeric. Existing `vod:ID`, `lti:ID`, `panopto:ID`
and `panoptocourseembed:ID` identify Moodle activities. Collections cannot be
downloaded as a single video; choose a recording ref.

`videos download REF --out PATH` publishes a private file without overwriting.
It defaults to a 4 GiB media cap, a 14400-second (4 h) overall download/remux
budget and a 60-second stall timeout: `--max-bytes` permits up to 16 GiB,
`--max-seconds` up to 86400 and `--stall-seconds` 1-600. A transfer that
waits for response headers or the next bytes beyond the stall timeout, or
outlives `--max-seconds` (including the final remux copy), fails with
NETWORK_ERROR; byte caps fail with LIMIT_EXCEEDED. There is no resume.
Remuxing uses a private (0700), exclusively created
`.klms-video-<pid>-<nonce>.scratch` directory beside `--out`; direct MP4 copies
need no scratch directory. Publication uses a private
`.klms-video-<pid>-<nonce>.part` file. Both are removed on normal or error
returns. An interrupted download (Ctrl-C or kill) may leave ffmpeg running
until it finishes or fails; the next download sweeps generated leftovers older
than 24 hours, restricted to the same owner on Unix. Sweeps skip symlinks and
unrelated names.
User downloads may fall back to a non-atomic exclusive copy when hard links
fail, with a warning; object-store publication requires an atomic hard link.
A remux peaks at about twice the media size on that filesystem; free space is not checked in
advance. Native VOD uses only static same-origin `/vod1/` MP4 sources, which
receive the KLMS session cookie and nothing else. Panopto exchanges use
separate host-scoped transient credentials; Panopto media (CDN) requests
receive neither KLMS nor Panopto credentials. Temporary signed media URLs are
never returned in JSON; Panopto `source_url` is the KLMS launch locator. HLS,
multi-track and offset recordings are retrieved under bounded transport
policies and remuxed locally with ffmpeg, whose presence is checked before any
media is fetched under a two-second check deadline; ffmpeg receives only local
paths. Remux errors include at most 4 KiB of stderr, and failed downloads kill
and reap ffmpeg. No player scripts or playback/progress telemetry run.
Multiple native Panopto streams are preserved as separate video/audio tracks
with their start offsets; a readable composed podcast takes precedence.
`stream_count` and `output_composition` expose this choice on inspected recordings.
Finite HLS playlists, external audio renditions, initialization fragments and
byte ranges are supported. Encrypted or unfinished/live playlists fail visibly.
Library sync remains metadata-only for videos, even with `--files`.
