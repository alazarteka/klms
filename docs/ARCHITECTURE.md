# Architecture

One crate, one binary. `main.rs` parses arguments, runs `commands::run`, and
prints the result through `output`. Arrows below point from a module to the
modules it uses:

```text
main         -> cli, commands, output, error
cli          -> auth (argument enums), models (Semester)
commands     -> cli, auth, client, board_pages, course_pages, parse, corpus, update, spec,
                models, date, reference, private_fs, safe_url, url, http, output, error
spec         -> cli, output, error
update       -> client, private_fs, digest, output, error
corpus       -> client, board_pages, course_pages, parse, models, date, url, reference,
                private_fs, digest, http (bounded reader), error
board_pages  -> client, parse, models, reference, url, error
course_pages -> client, parse, models, url, error
auth         -> http, parse, url, date, private_fs, output, error
client       -> auth (cookie jar only), http, parse, url, reference, private_fs, error
parse        -> models, reference, safe_url, date, url, error
reference    -> url, error
safe_url     -> url
output       -> models, error
models, date, digest, private_fs, url, error -> no internal module dependencies
```

## Modules

- `main`: entry point; reports clap parse errors in the JSON envelope under `--json`.
- `cli`: grammar and help text only. `spec` renders it as `klms spec`
  (and `klms --json spec`); `completions` generates shell scripts.
- `commands`: validates a job, coordinates transport and parsing, and builds
  the command result. `mod.rs` holds bootstrap, `Ctx`, and dispatch. Private
  `auth`, `courses`, `agenda`, `resources`, `preview`, `library`, and `videos`
  modules own the workflows. `download` shares file/video publication and
  result construction, preserving each command's diagnostics.
- `client`: KLMS URL policy, cookie selection, timeouts, redirects, response
  bounds, and authentication checks. `client/policy.rs` owns the read endpoint,
  identity-query, and redirect rules. It also has a separate unauthenticated
  HTTPS client for release downloads used by `update`.
- `client/stream`: the one bounded copy loop for video bodies (native and
  Panopto): byte caps, MP4/HTML sniffing before writing, the overall deadline
  and the stall timeout.
- `client/panopto`: transient Panopto delivery/folder reads and credential-free
  media transport, including bounded HLS retrieval and local ffmpeg remux.
  `Config` names the trusted Panopto/CDN origins and the ffmpeg executable
  (fixed in production); `Session` remains the facade. Private `delivery`
  decodes metadata and selects sources, `media` enforces CDN policy and
  coordinates downloads, `hls` localizes playlists, and `remux` owns ffmpeg
  processes. `exchange` performs the fixed LTI handoffs with exact host/path
  policies and an in-memory `auth::Jar`; no third-party credential is
  persisted. `folder` holds typed folder requests and completeness handling.
  Media agents remain per-resource so each redirect chain uses the remaining
  download deadline as its global timeout.
- `http`: the one blocking HTTP transport. It never follows redirects itself;
  each caller supplies its origin policy to `follow`. It also owns form
  payloads and bounded byte accumulation; callers retain size limits, text
  decoding, and error classification.
- `url`: the standard `url` crate's URL type, plus shared loopback, userinfo,
  query and form helpers. Moodle module paths and UUID validation belong in
  `reference`.
- `private_fs`: private file and scratch-directory creation, XDG/home path
  mechanics, and no-overwrite publication. `publish_atomic` requires hard
  links for corpus objects; `publish_download` reports its exclusive-copy
  fallback. Download publication alone runs the generated-name stale sweep:
  `.klms-download-<pid>-<hex nonce>.part[.<hex retry>]` or `.scratch`, older
  than 48 hours, with matching type and (on Unix) owner. Symlinks are skipped.
  Scratch directories are removed when their owner is dropped.
- `digest`: SHA-256 and lowercase hexadecimal formatting, shared by corpus
  and the release updater.
- `auth`: orchestration (`login`, `logout`, `forget`, `load`) and the status
  models it exposes, with submodules:
  - `flow`: the KAIST SSO state machine and prompts.
  - `transport`: the short-lived, exact-origin login transport.
  - `cookies`: cookie jar handling for the SSO exchange.
  - `store`: session file, remembered login and private file writes.
  - `secret`: password storage backends (macOS keychain, Linux Secret Service,
    or an opt-in plaintext file).
  - `tests`: auth tests.
  Serialized status models are secret-free. `auth` does no Panopto network
  work; `client/panopto` borrows only its cookie jar.
- `parse`: all KLMS/Moodle markup knowledge. Selectors live here and nowhere
  else. Submodules: `auth`, `calendar`, `courses`, `coursework`, `detail`
  and `shared`, plus static `native_video`, `embedded_video` and Panopto `video`
  launch parsers.
- `board_pages`: the one bounded board traversal shared by live board/notice
  lists and library sync: board identity checks, cycle detection, post
  deduplication, read policy on each next page, and the 20-page budget.
- `course_pages`: course activities across every page KLMS splits a course
  into, merging the paged week format.
- `models`, `reference`, `date`, `safe_url`, `error`, `spec`: typed records
  (including the semantic `Semester` enum with its clap derive), canonical
  refs and endpoint mapping, Korea-time arithmetic, URL redaction, error
  codes and exit categories, and the grammar spec.
- `output`: the JSON envelope, list and detail rendering for human output, and
  terminal sanitization.
- `corpus`: the only module that touches SQLite or the object store. Submodules:
  `schema`, `sync`, `observe`, `curate`, `query`, `content`, `object_store`.
  `sync` owns acquisition, download frontiers, and transactions; `observe`
  owns observation upserts and missing-state writes through `Run`. `query`
  builds effective views and maintains search projections. `content` selects
  stored bytes and handles diagnostics, preview, and export. SQLite error
  conversion lives in `corpus/mod.rs`. Commands contain no SQL. Local library
  commands load no auth.
- `update`: stable release selection, checksum and candidate version checks,
  the single executable-replacement path (`__install`, shared with
  `scripts/install.sh`), and the deprecation notice for a leftover legacy skill (reported, never removed). Tests are in
  `update/tests.rs`.

## Boundaries

KLMS HTML is untrusted: parsers use explicit selectors and fail visibly when a
required shape changes. Ordinary reads use a same-origin HTTPS client with
cookies from the owned session file; off-origin redirects are rejected and
external links (LTI, Panopto, Zoom) are never followed with KLMS credentials.
Explicit video commands perform only the fixed Panopto LTI handoffs, with KLMS
cookies attached to KLMS callback origins alone. Separate clean clients fetch
allowlisted CDN media; ffmpeg reads only downloaded local media.
Login uses a short-lived transport limited to `sso.kaist.ac.kr` and
`klms.kaist.ac.kr` (HTTP loopback exists only for tests). Moodle AJAX methods
are an allowlist; there is no arbitrary POST. Nothing checks for updates or
sends analytics at startup.

These are the only hosts klms contacts, all over HTTPS:

| Host | Used by | Credentials |
|---|---|---|
| `klms.kaist.ac.kr` | every course read, sync and download | KLMS session cookie |
| `sso.kaist.ac.kr` | `auth login` only | SSO cookies, held for one login |
| `kaist.ap.panopto.com` | `videos show` and `videos download`: LTI handoff and delivery metadata | Panopto cookies and the embed's access token, never KLMS cookies |
| `s-cloudfront.cdn.ap.panopto.com` | `videos download` media | none |
| `api.github.com`, `github.com` | `update` only | none |

The Panopto origins are fixed in `src/client/panopto/mod.rs` (`Config::production`).
