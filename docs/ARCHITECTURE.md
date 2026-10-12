# Architecture

One crate, one binary. `main.rs` parses arguments, runs `commands::run`, and
prints the result through `output`. Arrows below point from a module to the
modules it uses:

```text
main      -> cli, commands, output, error
commands  -> cli, auth, client, course_pages, parse, corpus, update, spec, output
spec      -> cli, output
update    -> client, output
corpus    -> client, course_pages, parse, models, date, url, reference, error
course_pages -> client, parse, models, url
auth      -> http, parse, url, date, output, error
client    -> auth (cookie jar only), http, parse, url, private_fs, error
parse     -> models, reference, safe_url, date, url, error
output    -> models, error
```

## Modules

- `main`: entry point; reports clap parse errors in the JSON envelope under `--json`.
- `cli`: grammar and help text only. `spec` renders it as `klms spec`
  (and `klms --json spec`); `completions` generates shell scripts.
- `commands`: validates a job, coordinates transport and parsing, and builds
  the command result. `commands/library.rs` holds the local-library commands;
  `commands/videos.rs` coordinates explicit video discovery and downloads.
- `client`: KLMS URL policy, cookie selection, timeouts, redirects, response
  bounds, and authentication checks. It also has a separate unauthenticated
  HTTPS client for release downloads used by `update`.
- `client/stream`: the one bounded copy loop for video bodies (native and
  Panopto): byte caps, MP4/HTML sniffing before writing, the overall deadline
  and the stall timeout.
- `client/panopto`: transient Panopto delivery/folder reads and credential-free
  media transport, including bounded HLS retrieval and local ffmpeg remux.
  `Origins` names the Panopto and media CDN origins every policy trusts (fixed
  in production). `exchange` performs the fixed LTI handoffs with exact
  host/path policies and an in-memory `auth::Jar`; no third-party credential
  is persisted. `folder` holds typed folder requests and completeness handling.
- `http`: the one blocking HTTP transport. It never follows redirects itself;
  each caller supplies its origin policy to `follow`.
- `url`: a small `http`/`https` URL type with WHATWG-style normalization for a
  single-origin client.
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
- `course_pages`: course activities across every page KLMS splits a course
  into, merging the paged week format.
- `models`, `reference`, `date`, `safe_url`, `error`, `spec`: typed records,
  canonical refs and endpoint mapping, Korea-time arithmetic, URL redaction,
  error codes and exit categories, and the grammar spec.
- `output`: the JSON envelope, list and detail rendering for human output, and
  terminal sanitization.
- `corpus`: the only module that touches SQLite or the object store. Submodules:
  `schema`, `sync`, `curate`, `query`, `object_store`. Commands contain no SQL.
  Local library commands load no auth.
- `update`: stable release selection, checksum and candidate version checks,
  the single executable-replacement path (`__install`, shared with
  `scripts/install.sh`), and removal of the legacy managed skill. Tests are in
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
