# Architecture

`klms` is one Rust crate and one installed binary. Its dependency direction is:

```text
CLI -> commands -> authenticated client + parsers -> models -> output
```

## Boundaries

- `cli`: command grammar only.
- `commands`: validates a job and coordinates transport and parsing.
- `client`: URL policy, cookie selection, timeouts, redirects, response bounds,
  and authentication checks.
- `auth`: secret-file discovery and storage-state parsing without transport or
  rendering concerns.
- `parse`: upstream HTML knowledge. Selectors and Moodle-specific markup stay
  here.
- `models`: typed records returned by commands.
- `output`: stable JSON envelopes, human rendering, terminal sanitization, and
  exit categories.

The CLI does not introduce a provider framework, service container, plugin
registry, or persistent database before multiple real implementations require
one.

## Trust boundaries

KLMS HTML is untrusted input. Parsers use explicit selectors, preserve source
identities, and fail visibly when a shape required for correctness changes.
Human output sanitizes terminal control and bidirectional formatting
characters. JSON preserves encoded text but never contains credentials.

External activities such as LTI, Classum, Panopto, and Zoom are represented as
typed links. The client does not follow them across origins or inherit KLMS
credentials into them.

## Network behavior

Ordinary reads use a direct HTTPS client with cookies selected from a protected
Playwright-compatible storage-state file. A dashboard request establishes that
the session is authenticated. Commands then fetch the narrowest required page.
Redirects that leave the configured origin are rejected.

All requests have bounded response bodies and timeouts. Future fan-out is
bounded and deterministic. No command performs an update check or analytics
request during startup.

