# JSON contract

`--json` emits one compact document. Successful envelopes go to stdout and
errors go to stderr; consumers must inspect both the exit status and `ok`.

```json
{"schema_version":"4","ok":true,"command":"courses.list","data":[],"warnings":[],"meta":{"returned":0,"limit":100,"complete":true,"total":0,"next_cursor":null,"fresh_through":null,"source_complete":true}}
```

```json
{"schema_version":"4","ok":false,"error":{"code":"AUTH_REQUIRED","message":"...","hint":"...","retryable":false}}
```

Errors may include `details` when structured diagnostics help recovery. Raw
SSO responses, cookies, and other secrets are never included. An unhealthy
`doctor` result is an error with diagnostics under `error.details`.

Collection `meta.returned` is the number of records in `data`; `limit` is the
requested bound; and `complete` says whether that local result set was
truncated. `total` is present only when truthful. `next_cursor` is reserved
and currently always null.

For library search and changes, `source_complete` describes the latest
non-running global sync: `true` means complete source coverage, `false` means
incomplete coverage, and `null` means no evidence exists. A scoped sync never
claims global coverage. `fresh_through` is the finish time of the latest
complete, source-complete global sync. Library activity and history use null
source coverage because they are local timelines.

Canonical resource references include `course:ID`, `file:ID`,
`activity:KIND:ID`, `board-post:BOARD:POST`, `resource:HASH`,
`representation:N`, and `sha256:HEX`. Agents should pass these `ref` values to
follow-up commands rather than extracting URLs.

Library data shapes are:

```text
library.status   {database_path, object_store_path, schema_version, created,
                  courses, resources, representations, blobs, stored_bytes,
                  last_sync, fresh_through}
library.sync     {ref, status, source_complete, courses, resources,
                  representations, blobs_added, changes, truncated,
                  failures}
library.search   [{ref, kind, course_ref, title, snippet, has_content}]
library.changes  [{id, occurred_at, kind, subject_ref, before_ref,
                  after_ref, details}]
library.activity [{ref, subject_ref, field, value, actor, revision,
                  created_at, retracted}]
library.history  [{id, observed_at, kind, digest, source}]
library.content  {ref, byte_length, mime, filename, text, truncated}
library.export   {ref, path, byte_length}
library.edit     {ref, subject_ref, field, before, after, revision, actor}
library.retract  {ref, target_ref, actor}
library.relations.add {ref}
```

`library.status.last_sync` is null before the first attempt; otherwise it is
`{ref, scope, started_at, finished_at, status, source_complete}`. `scope` is
`all` or the selected course reference (an unresolved filter can remain on a
failed attempt). A persisted `running` status is projected as `unfinished`:
completion was not recorded, and process liveness is unknown. A warning tells
the caller to check the original process before retrying. No database rows or
finish timestamps are rewritten by this projection.

Local collection truncation and sync failures also populate `warnings`.
Partial syncs still return a successful envelope and exit 0, with
`data.status: "incomplete"` and `data.failures`; these must not be mistaken for
complete source coverage. Human status/list output now exposes these same
qualifications. The envelope remains version `"4"` and SQLite remains version 1.

Interface discovery shapes are:

```text
version      {name, version}
help         {text}
update       {current_version, latest_version, update_available, updated, path}
spec         {name, version, global_args: [ARG], commands: [{path: [String],
              usage, about, args: [ARG], groups: [GROUP]}]}
             ARG = {name, kind: positional|flag|option, required, value,
                    choices: [String], default, help}
             GROUP = {name, args: [String], required, multiple}
completions  {shell, script}
```

`--json --version` and explicit `--json ... --help` requests are successful
envelopes on stdout. `update.current_version` is the running version before
the operation; `latest_version` is the newest published stable release. Both
omit the tag's `v` prefix. `path` is the resolved executable target.
`updated` is true only after successful installation, and `--check` always
leaves it false. An installation failure is a nonzero error envelope; errors
from the candidate installer are retained under `error.details.candidate_error`.

`spec` mirrors the executable Clap declaration; `usage` is the same line that
`klms spec` prints without `--json`. Hidden arguments, `--help`, `--version`,
and the `help` subcommand are omitted. A group with `required` true needs at
least one member; `multiple` false allows at most one, and such a group renders
in `usage` as `(a|b)` or `[a|b]`.

`library.sync.truncated` counts resources whose detail page hit a parser cap
(100,000 text characters or 100 links). Those observations are stored as
incomplete and their representations are never marked missing, but the run
itself still completes; consumers that care about bounded coverage should
check this count rather than `status`.

`library.show` varies by reference kind. Course, resource, and representation
results keep source fields separate from `effective` curation and report
active assertion provenance under `effective._provenance`. Summaries include
`summary_stale`. Resources list their representations; representations expose
only their own current content; SHA-256 results list representations that have
observed those exact bytes.

Representation history interleaves `representation_source` and
`verified_content`. Verified entries include a directly usable `sha256_ref`,
URL, validators, byte length, MIME, and `sync_ref`. A→B→A creates three
observations while storing two blobs. Corresponding remote byte changes use
exact `sha256:` values in `before_ref` and `after_ref`.

Content/export refer to downloaded file bytes, not source-observation text.
Use `library show REF` and `data.source.text` for stored notice text. Missing
bytes retain error code `CONTENT_UNAVAILABLE` (exit 55), with hints to read
source text, inspect non-file link metadata (`data.source.url`), or explicitly
sync recorded file candidates for the relevant course. Non-file links and
resources without recorded file candidates do not receive download-and-retry
hints. Previously missing files point to their parent's observation state;
the error does not claim that no remote file exists. Existing stored bytes
take precedence over these hints, including historical content. A notice with
no downloaded bytes lists up to 20 present file candidates in ascending
representation-ID order under `error.details.representations` (reference
strings), with filenames and the scoped download command in the hint. The
hint retains the stored-text reading option when text exists and explicitly
notes if the candidate list is truncated. Links and not-observed attachments
are excluded. Bounded
content reports `truncated` inside the `library.content` data object,
not in the envelope. Export verifies the SHA-256 and refuses an existing
destination. Ambiguous resource content returns `CONTENT_UNAVAILABLE` with
candidate representation refs under `error.details`.

The machine contract remains experimental during `0.x`. Consumers should
check both `schema_version` and the installed binary version.

Numeric library references accepted with leading zeros resolve to the same
canonical identity before curation is stored. Truncated UTF-8 content previews
omit an incomplete trailing code point instead of classifying valid text as
binary. A validator refresh with unchanged bytes can append a verified-content
observation without adding a blob or emitting `verified_content_changed`.
