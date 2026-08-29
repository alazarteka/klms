# JSON contract

`--json` emits exactly one compact JSON document. Standard output is reserved
for successful envelopes; standard error receives error envelopes. Consumers
must inspect both the process exit status and `ok`.

Successful commands use:

```json
{"schema_version":"1","ok":true,"command":"courses.list","data":[],"warnings":[]}
```

Errors use:

```json
{"schema_version":"1","ok":false,"error":{"code":"AUTH_REQUIRED","message":"...","hint":"...","retryable":false}}
```

The machine contract is experimental during the `0.x` series. Consumers must
check both `schema_version` and the installed binary version. We prefer one
documented schema correction over preserving misleading semantics; once the
typed resource model settles, incompatible changes will require a new
`schema_version`.
