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

Fields may be added within schema version `1`; existing fields will not change
meaning. A breaking shape change requires a new `schema_version`.
