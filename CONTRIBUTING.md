# Contributing

Open an issue before broadening the command surface or introducing a new
dependency. Small parser and correctness fixes can go directly to a pull
request with a redacted fixture or focused synthetic test.

Before sending a change, run:

```bash
make check
./scripts/cargo-deny.sh check   # when dependencies change
```

Live checks must remain read-only and must not print or commit private course
data. See `SECURITY.md` for vulnerability reports and dependency policy.
