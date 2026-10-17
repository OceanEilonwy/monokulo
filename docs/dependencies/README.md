# Dependency catalog

`catalog.sqlite` lists every first-level Rust dependency of the workspace
(and of `fuzz/`): who declares it, every file that uses it, what dropping it
would remove from the build and from the shipped binaries, and a proposal on
whether to drop it. Dependencies of dependencies are never catalogued, only
counted as weight.

Rebuild it after dependencies or their usage change:

    python3 scripts/dependency-catalog.py

Binary sizes come from `cargo bloat`; without `--bloat` the last measured
sizes are kept. To measure again (monokulo needs `npm ci` first):

    cargo bloat --release -p monokulo --bin monokulo --crates -n 0 --message-format json > monokulo.json
    # likewise engine/monokulo-engine, key-custody-cli, cli-wallet/wallet-cli
    python3 scripts/dependency-catalog.py --bloat monokulo=monokulo.json ...

The proposals are kept in `proposals.json`: edit it there and rebuild.

## Tables

- `dependencies`, `declarations`, `crates`: what each crate's Cargo.toml names.
- `usages`: one row per file that refers to a dependency. `path_refs` counts
  lines naming `dep::…`, `symbol_refs` lines naming something imported from
  it (by name, so approximate).
- `usage_sites`: every such line.
- `dependency_weight`: crates that leave the build when this one dependency
  goes, one at a time (clap weighs nothing while clap_complete needs it).
- `binary_size`: `.text` bytes those crates contribute to each release
  binary. C and C++ code (bundled SQLite, RandomX, aws-lc) isn't attributed.
- `proposals`: the review, `review_rank` 1 being the least used.
- `usage_rank` (view): crates, files and references per dependency.

## Queries

    -- The review, least used first
    SELECT review_rank, dependency, difficulty, recommendation, size_impact
    FROM proposals ORDER BY review_rank;

    -- Where a dependency is used
    SELECT file, line, text FROM usage_sites WHERE dependency = 'zeromq';

    -- Declared but never referenced
    SELECT crate, dependency FROM declarations d WHERE NOT EXISTS
      (SELECT 1 FROM usages u WHERE u.dependency = d.dependency AND u.crate = d.crate);
