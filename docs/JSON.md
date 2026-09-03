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

Bounded content reports `truncated` inside the `library.content` data object,
not in the envelope. Export verifies the SHA-256 and refuses an existing
destination. Ambiguous resource content returns `CONTENT_UNAVAILABLE` with
candidate representation refs under `error.details`.

The machine contract remains experimental during `0.x`. Consumers should
check both `schema_version` and the installed binary version.
