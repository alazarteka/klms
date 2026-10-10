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
client    -> http, url, error
parse     -> models, reference, safe_url, date, url, error
output    -> models, error
```

## Modules

- `main`: entry point; reports clap parse errors in the JSON envelope under `--json`.
- `cli`: grammar and help text only. `spec` renders it as `klms spec`
  (and `klms --json spec`); `completions` generates shell scripts.
- `commands`: validates a job, coordinates transport and parsing, and builds
  the command result. `commands/library.rs` holds the local-library commands.
- `client`: KLMS URL policy, cookie selection, timeouts, redirects, response
  bounds, and authentication checks. It also has a separate unauthenticated
  HTTPS client for release downloads used by `update`.
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
  Only non-secret status models leave this module.
- `parse`: all KLMS/Moodle markup knowledge. Selectors live here and nowhere
  else. Submodules: `auth`, `calendar`, `courses`, `coursework`, `detail`
  and `shared`.
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
Login uses a short-lived transport limited to `sso.kaist.ac.kr` and
`klms.kaist.ac.kr` (HTTP loopback exists only for tests). Moodle AJAX methods
are an allowlist; there is no arbitrary POST. Nothing checks for updates or
sends analytics at startup.
