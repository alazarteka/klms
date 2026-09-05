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

The supply-chain checks use uv 0.12.10 (the CI version) and Python 3.11 or
newer. Their YAML parser is declared in `tests/supply_chain_contract.py` and
pinned, including archive hashes, in the adjacent `.lock` file. To intentionally
change it, edit the declaration and run `uv lock --script
tests/supply_chain_contract.py`. These are development tools, not CLI dependencies.

Workflow policy requires all three dependency gates as unconditional, standalone
steps in each job that invokes Cargo, in their existing order. The checker reads
YAML jobs and steps; it does not interpret shell scripts or accept gates inherited
from another job. Behavioral fixtures exercise accepted and rejected workflows.
