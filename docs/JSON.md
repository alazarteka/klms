# JSON contract

`--json` emits exactly one compact JSON document. Standard output is reserved
for successful envelopes; standard error receives error envelopes. Consumers
must inspect both the process exit status and `ok`.

Successful commands use:

```json
{"schema_version":"2","ok":true,"command":"courses.list","data":[],"warnings":[],"meta":{"returned":0,"limit":100,"complete":true,"total":0,"next_cursor":null}}
```

Errors use:

```json
{"schema_version":"2","ok":false,"error":{"code":"AUTH_REQUIRED","message":"...","hint":"...","retryable":false}}
```

Collection commands include envelope-level `meta`. `returned` is the number of
records in `data`; `limit` is the requested bound; `complete` says whether the
result contains every record known from the fetched KLMS page; `total` is null
when upstream pagination prevents a truthful count; and `next_cursor` is null
until KLMS exposes a cursor the client can safely continue. An empty recognized
page is a successful collection with `returned: 0`. An unrecognized page shape
is `UPSTREAM_SHAPE_CHANGED`, never a successful empty list.

Resource records use the field `ref` for a canonical, round-trippable reference
such as `course:180871`, `assign:1210516`, `quiz:1210482`,
`board-post:1189554:439261`, or `file:1205160`. URLs remain available as source
evidence, but agents should pass `ref` to follow-up commands.

`files.download` streams into a protected temporary file, atomically publishes
it without replacement, and returns an absolute path, byte count, final
same-origin source URL, and content type. Large file bytes never enter JSON.

The machine contract is experimental during the `0.x` series. Consumers must
check both `schema_version` and the installed binary version. We prefer one
documented schema correction over preserving misleading semantics; once the
typed resource model settles, incompatible changes will require a new
`schema_version`.
