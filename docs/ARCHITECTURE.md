# Architecture

One crate, one binary. Dependency direction:

```text
cli -> commands -> client + parse -> models -> output
              \-> corpus -> SQLite + object store
              \-> update -> release client + installer
```

## Modules

- `cli`: grammar and help text only. `spec` renders it as `klms spec`.
- `commands`: validates a job, coordinates transport and parsing.
- `client`: URL policy, cookie selection, timeouts, redirects, response bounds,
  authentication checks; plus a separate unauthenticated HTTPS client for
  `update`. `http` and `url` are the small HTTP/URL primitives under it.
- `auth`: the KAIST SSO state machine, exact-origin login transport, prompts,
  remembered login, password backends, and private session persistence. Exposes
  only non-secret status models.
- `parse`: all KLMS/Moodle markup knowledge. Selectors live here and nowhere
  else. `course_pages` merges paged week formats.
- `models`, `reference`, `date`, `safe_url`, `error`: typed records, canonical
  refs and endpoint mapping, Korea-time arithmetic, URL redaction, error codes.
- `output`, `present`: JSON envelope, exit categories, sanitized human output.
- `corpus`: the only module that touches SQLite or the object store (queries,
  curation, sync). Commands contain no SQL. Local library commands load no auth.
- `update`: stable release selection, checksum and candidate version checks,
  and the single executable-replacement path (`__install`, shared with
  `scripts/install.sh`), plus removal of the legacy managed skill.

## Boundaries

KLMS HTML is untrusted: parsers use explicit selectors and fail visibly when a
required shape changes. Ordinary reads use a same-origin HTTPS client with
cookies from the owned session file; off-origin redirects are rejected and
external links (LTI, Panopto, Zoom) are never followed with KLMS credentials.
Login uses a short-lived transport limited to `sso.kaist.ac.kr` and
`klms.kaist.ac.kr` (HTTP loopback exists only for tests). Moodle AJAX methods
are an allowlist; there is no arbitrary POST. Nothing checks for updates or
sends analytics at startup.
