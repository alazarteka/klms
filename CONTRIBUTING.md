# Contributing

Open an issue before broadening the command surface or introducing a new
dependency. Small parser and correctness fixes can go directly to a pull
request with a redacted fixture or focused synthetic test.

Run the supply-chain gates before compiling dependency changes, then run:

```bash
make check
./tests/supply_chain_contract.sh
```

Live checks must remain read-only and must not print or commit private course
data. See `SECURITY.md` for vulnerability reports and dependency policy.
