# Code audit

A review of the workspace's Rust code (not `e2e/` or `crates/e2e-harness`)
for **bugs**, **ways to simplify** and **better type safety**, run by several
agents at once. They share one SQLite database through `audit.py`.

- `schema.sql`: the tables (crates, files, modules, reviews, findings, jobs, tool signals).
- `modules.toml`: the app's features and which files belong to each.
- `audit.py`: every read and write. `audit.py --help` lists the commands.

The database is `target/code-audit/audit.db` in the main checkout, so every
worktree sees the same one. It was set up with:

```sh
audit.py init && audit.py inventory && audit.py modules
audit.py signals target/code-audit/clippy.json target/code-audit/machete.txt
audit.py plan
```

`clippy.json` is `cargo clippy --workspace --exclude e2e-harness --lib --bins
--message-format=json` with extra lints (casts, unwrap/expect/panic,
indexing, string slicing, excessive bools, needless pass by value, too many
lines), run with `MONOKULO_POS_BUILD=none`.

## How an agent works

Give yourself a name and keep it: `--agent a3` on every command. Then loop:

1. `audit.py claim --agent NAME` prints your next job's brief, or `NO JOB`.
   Verify jobs come first, then module reviews, and the cross-cutting themes
   start once every module review is done.
2. Do the job (below), reporting as you go.
3. Finish it: `done` for a review, `verify` for a verify job.

A claim lasts 90 minutes, and each command on the job extends it. If you
can't finish, `release` the job so someone else picks it up.

**The code is read-only.** Read it from the audit's worktree at the commit
the brief names. Don't edit, build or run tests: five agents share the
machine. Use `rg`, `git grep` and reading.

### A review job (module or cross-cutting)

- Read every target file in full. Search the context files (tests, helpers)
  to see what's covered and how the code is called; don't review them.
- Hunt for, in this order:
  1. **bugs**: wrong results, panics reachable from input, races, lost or
     doubled money, broken state transitions, security holes, privacy or Tor
     leaks (always critical);
  2. **type safety**: types that would make a bug impossible (newtypes for
     IDs and amounts, enums for bools and strings, `Option<Option>`, errors
     callers can't match);
  3. **simplification**: duplicated logic, needless layers, code that does
     in 80 lines what 20 would; and **dead code** (code only tests call).
  4. **convention**: breaking the project's own rules (see the
     `house-rules` theme in `audit.py`).
- Triage the brief's tool signals. Most `indexing_slicing` and `expect` hits
  are fine; report the ones input can actually reach.
- Don't repeat what the brief lists as already reported.
- Record each finding when you find it:

  ```sh
  audit.py finding --agent NAME --job N --file crates/x/src/y.rs --lines 120-134 \
    --category bug --severity high --confidence medium \
    --title "Short claim" \
    --problem "What is wrong and why" \
    --scenario "Concrete input or state, and the wrong outcome it gives" \
    --recommendation "The fix, specifically" \
    --test "The regression test that would catch it"
  ```

  Text with backticks, `#[...]` or quotes can trip the shell's checks. Then
  write the fields as a JSON object to a file under
  `/home/henry/.claude/jobs/662be1eb/tmp/` (keys as the flags: `job`, `file`,
  `lines`, `category`, ..., `test`) and run
  `audit.py finding --agent NAME --from FILE`. `verify` and `note` take
  `--from` too (`job`, `verdict`, `note`, `duplicate_of`, `severity`; or
  `finding`, `text`).

  Paths are relative to the repository root. Line numbers are at the audited
  commit. `--scenario` is required: if you can't say what goes wrong, it
  isn't a bug finding (it may still be `simplify` or `type_safety`, where
  the scenario is the change that becomes impossible or the code that
  disappears).

  Severity: **critical** = money lost, keys or privacy leaked, remote crash;
  **high** = wrong behaviour a user will hit; **medium** = an edge case, or a
  type-safety change that removes a class of bugs; **low** = cleanups.
- To add to a finding already recorded (a wider impact, a related place):
  `audit.py note --agent NAME FINDING "text"`.
- For a module review, note each target file's purpose in one line:
  `audit.py purpose PATH "what it is for"`. `done` refuses until they're set.
- Close with `audit.py done --agent NAME --job N --summary "..."`: what you
  covered, what looked solid, and anything you ran out of time for.

### A verify job

You're checking another agent's finding. Try to prove it wrong.

- Read the code at the lines given and around them, and follow the calls.
  Does the failure scenario actually happen? Is it already handled
  elsewhere, by a caller, a type, a database constraint or a test?
- Check the brief's list of other findings in the file for a duplicate.
- Settle it:

  ```sh
  audit.py verify --agent NAME --job N --verdict confirmed|rejected|duplicate \
    --note "Why, with file:line evidence" [--duplicate-of M] [--severity S]
  ```

  Correct the severity with `--severity` if it's wrong either way.

## Watching and reporting

```sh
audit.py status                     # jobs and findings by state
audit.py findings --module engine-store
audit.py report --out target/code-audit/report.md
audit.py query "SELECT ..."         # read-only SQL
```

The tables to query are `review_finding` (findings), `job`, `review`,
`review_file`, `crate_file`, `module`, `module_file` and `signal`; see
`schema.sql`.

Run each command written out in full: the worktree's shell refuses loops
and commands held in variables.
