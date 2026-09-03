# Architecture

`klms` is one Rust crate and one installed binary. Its dependency direction is:

```text
CLI -> commands -> authenticated client + parsers -> models -> output
              \-> corpus -> SQLite + object store
              \-> local skill installer
```

## Boundaries

- `cli`: command grammar only.
- `commands`: validates a job and coordinates transport and parsing.
- `client`: URL policy, cookie selection, timeouts, redirects, response bounds,
  and authentication checks.
- `auth`: the typed KAIST SSO state machine, exact-origin login transport,
  transient cookies, SEED-CBC request encoding, terminal prompts, and private
  owned-session persistence. It exposes only non-secret status models.
- `parse`: upstream HTML knowledge. Selectors and Moodle-specific markup stay
  here.
- `models`: typed records returned by commands.
- `reference`: canonical resource-reference parsing and endpoint mapping.
- `date`: narrow KLMS date normalization and Korea-time window arithmetic.
- `present`: scannable human representations of typed records.
- `output`: versioned JSON envelopes, human rendering, terminal sanitization, and
  exit categories.
- `corpus`: the only module allowed to touch SQLite or the object store. It
  owns local-library queries, curation, and sync transactions; commands never
  embed SQL. Purely local operations do not load authentication.
- `skill`: embedded companion-skill payload, XDG data placement, and the
  cross-client discovery symlink. It has no network or KLMS authentication
  dependency.

The private versioned library is the CLI's one persistent store. No provider
framework, service container, or plugin registry is introduced.

Persistent state is limited to explicitly invoked local features: a mode-0600
owned-session record, the installed companion skill, and the private versioned
library described in `docs/LOCAL_LIBRARY.md`. Moodle session keys are kept only
in memory. Consequently, `auth time-left` bootstraps from the dashboard and
discloses that this read may refresh activity time.

## Trust boundaries

KLMS HTML is untrusted input. Parsers use explicit selectors, preserve source
identities, and fail visibly when a shape required for correctness changes.
Human output sanitizes terminal control and bidirectional formatting
characters. JSON preserves encoded text but never contains credentials.

External activities such as LTI, Classum, Panopto, and Zoom are represented as
typed links. The client does not follow them across origins or inherit KLMS
credentials into them.

## Network behavior

Ordinary reads use a strict same-origin HTTPS client with cookies from the owned
session file. A dashboard request establishes that the session is
authenticated. Commands then fetch the narrowest required page.
Redirects that leave the configured origin are rejected. These operations are
content-read-only: they do not submit coursework or change course data, though
KLMS itself may refresh its session activity timer when serving an authenticated
page. Commands that make a diagnostic bootstrap read disclose that effect.

All requests are bounded while streaming and have configurable, capped
timeouts. Moodle AJAX methods are fixed in a client-side allowlist; arbitrary
POST is not exposed. Future fan-out is bounded and deterministic. No command
performs an update check or analytics request during startup.

Login uses a separate, short-lived blocking transport. In production it permits
only `sso.kaist.ac.kr` and `klms.kaist.ac.kr`; HTTP loopback origins exist only
for integration tests. It implements bounded redirects and cookie selection
itself so central SSO state can be discarded after the final KLMS cookie is
issued. Protocol result codes are mapped to typed transitions and unknown
codes fail closed.

## Companion skill

The release binary embeds the repository's `skills/klms/SKILL.md`. `skill
install` writes that exact payload under `$XDG_DATA_HOME/klms/skills/klms` (or
`~/.local/share/klms/skills/klms`) and links it from `~/.agents/skills/klms`.
It never downloads mutable skill content at runtime and refuses to replace an
unexpected discovery path.
