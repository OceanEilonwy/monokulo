# Packaging the push relay and the Bell: project report

Written 2026-10-29 by the principal Rust engineer of the team (a principal,
two Rust engineers, an OpenWrt/LuCI engineer and a QA lead) that moved the
Monokular Bell into this repository and gave it and the push relay what a
service manager and a router need, while the owner was away. Nothing was
asked of the owner: every call is numbered in `docs/decisions/bell.md`
(59 to 74, the move), `docs/decisions/packaging.md` (1 to 44, the
packaging) and `docs/decisions/push.md` (161 to 163, the relay's
cross-references), each with its reason and what else was possible, so any
of them can be reversed knowingly. This report is the way in: what was
built and where, how to install it, every decision in a line, the tests
and gates, the licence flag, and what is left.

The brief was the owner's plan, not re-decided here: the Bell moves from
monokular into monokulo as the crate and binary `monokulo-bell`, with its
history replayed linearly; the relay keeps its name `monokulo-push-relay`;
both get `--check` and a local status endpoint; release zips for monokulo's
five targets with service files; one OpenWrt package per service with its
LuCI page inside, in the one signed apk feed, tested in the OpenWrt 25.12
rootfs; docs and decision logs. monokulo `main` was `a5845fce4` when the
plan was written and moved sixty-odd commits of other work while the
stages ran; every stage rebased onto it and merged linearly.

## 1. In short

`monokulo-bell` and `monokulo-push-relay` are now the two services that run
beside Monokulo, both from this repository, both installable three ways:

- **On an OpenWrt router**, from the apk feed at `docs.monokulo.com/openwrt/`
  that already held `monokulo`: `apk add monokulo-push-relay` or `apk add
  monokulo-bell`. Each package carries its binary, a procd init script, its
  UCI config, a root-only secrets folder (the one secret it can make is made
  on install), a data folder in `/srv/<name>` kept across `sysupgrade`, a
  firewall rule made off, and its own LuCI page under Services, whose save
  runs the binary's `--check` and shows the binary's own words when it
  refuses. Built for `aarch64_cortex-a53` (the owner's GL.iNet Flint 2) and
  `x86_64`; CI boots the x86_64 rootfs and installs, configures, upgrades
  and removes all three packages there.
- **On a computer**, from the release zips `monokulo-push-relay-<version>-<target>.zip`
  and `monokulo-bell-<version>-<target>.zip` for x86_64 and aarch64 Linux,
  aarch64 and x86_64 macOS and x86_64 Windows, each with the binary, its
  README, a starter options file its own `--init` wrote, a `.sha256`, and
  `service/` with a systemd unit, a launchd daemon and a Windows startup
  task script. Every release runner installs and starts the zip's service
  file before the zip is kept.
- **In a terminal**, as before: `--init`, `--check`, run.

Both binaries have `--check` (the first half of a start: every setting and
secret checked exactly as a start checks them, nothing opened or bound,
`OK` and exit 0 or the start's own words on stderr and exit 1) and a
status listener on loopback (`127.0.0.1:8446` and `127.0.0.1:8447`, `GET
/status`: where it listens, its public address, what it serves and whether
each is ready, how many handles or registrations it holds, and the last
push or ding; never a token, endpoint or payment). Neither serves TLS: each
needs an https reverse proxy in front, and the router packages, the docs
and the LuCI pages say how (Caddy, nginx).

The monokulo binary never depends on `monokulo-bell`: lint.yml asserts it
over `cargo tree -p monokulo --all-features`. The Bell's secrets are
`MONOKULO_BELL_*`; the `MONOKULAR_BELL_*` names are still read for one
release, each with a warning at start. monokular no longer holds the Bell;
its app's Bell client is unchanged.

## 2. What was built and where

Seven pull requests, each rebase-merged onto monokulo `main` once every
check on its final head passed (monokular's one squash-merged, as that
repository does). All on 2026-10-29.

| PR | Stage | Merge commit | What it holds |
| --- | --- | --- | --- |
| monokulo [#257](https://github.com/OceanEilonwy/monokulo/pull/257) | 1, the move | `51dfea123` (12 commits, `97f853740` to `51dfea123`) | monokular's five Bell commits replayed with only the Bell's paths (`crates/monokulo-bell`, `tools/fake-square`, `tools/fake-clover`, `docs/bell.md`, `docs/decisions/bell.md`), authors and dates kept, each message naming its monokular PR and source commit; then the Bell as a workspace member on path dependencies, logging through `telemetry` with a `logging.format` setting, `MONOKULO_BELL_*` with `live_settings::Env::with_renamed` for the old names, lint.yml's production clippy of the Bell and the "Monokulo never depends on the Bell" check, the Docker image building it, the `/bell/` docs page (API, problem types as `…/bell/#<slug>`, "Run one"), `crypto-bigint` 0.7.10 as a security exception to the two-week rule. Decisions bell.md 59 to 73, push.md 161. |
| monokular [#22](https://github.com/OceanEilonwy/monokular/pull/22) | 1, the removal | `16dee74f` (squash) | `crates/monokular-bell`, `tools/fake-square`, `tools/fake-clover`, `docs/bell.md` and `docs/decisions/bell.md` removed; the workspace, `rust.yml` and the root `[workspace.dependencies]` on monokulo's git crates dropped; pointers left in the README, `docs/design.md` and the report. The Android app and its decision log untouched. Merged after #257 landed. |
| monokulo [#259](https://github.com/OceanEilonwy/monokulo/pull/259) | 2, check and status | `8fc440c5f` (`e694f0210`, `7f0540178`, `8fc440c5f`) | `--check` for both binaries (`Relay::check`, `Bell::check`, `live_settings::cli::with_check`), the status listener (`live_settings::LoopbackAddr`, `service_runtime::status` behind a `status` feature, the `Host` check), `--version` naming the commit, monokulo refusing `[relay.status]`; the `/push-relay/` and `/bell/` pages' "Check before starting", "Status" and "TLS"; `docs/decisions/packaging.md` 1 to 12, push.md 162, bell.md 74. |
| monokulo [#260](https://github.com/OceanEilonwy/monokulo/pull/260) | 2, a CI fix | `a59b4edfd` | The Bell's problem-heading test compared against `include_str!`'d `bell.md` with `\n`, which a Windows checkout reads with CRLF: Windows CI had been red since #257. Now line by line. |
| monokulo [#261](https://github.com/OceanEilonwy/monokulo/pull/261) | 3, release zips | `474680a70` (`7b336220e`, `8cd7bd913`, `474680a70`) | The two READMEs; `crates/<crate>/service/` (systemd unit, launchd plist, `install-windows-task.ps1`), each held to the binary's own options and secrets by a unit test; release.yml's `services` matrix job (ten jobs: two binaries, five targets, x86_64 macOS on `macos-15-intel`), `scripts/check-service-files.sh` installing and starting each zip's service file on its runner and reading `/status`, the release notes' table; `packaging.md` 13 to 20, push.md 163. |
| monokulo [#263](https://github.com/OceanEilonwy/monokulo/pull/263) | 4, OpenWrt | `105033fa4` (7 commits, `66d3d40bd` to `105033fa4`) | `openwrt/monokulo-push-relay/` and `openwrt/monokulo-bell/` (Makefile, init, UCI config, keep file, uci-defaults firewall rule, LuCI view, menu and ACL named `luci-app-<package>`), `openwrt/common/` (`service.sh`, `rpcd.uc`: the init scripts' shared half and the pages' rpcd ucode backend), `scripts/build-openwrt.sh` building all three into one feed (a throwaway key when there is no real one), `scripts/test-openwrt.sh` (the rootfs suite), openwrt.yml's x86_64 matrix entry and `test` job, `crates/<crate>/tests/openwrt.rs` holding each package to its binary's settings; monokulo's own LuCI page reading procd through the package's backend instead of `service list` (which exposed the encryption key to the browser); QA's fixes (every refused start says why on the page; the refusal names the operator's settings, not the generated file; each page restarts only its own service); `packaging.md` 21 to 40. |
| monokulo (this PR) | 5, docs | see the PR | The `/services/` page (`xtask/src/pages/install/services.rs`: both services on a router and a computer, the TLS proxy, status and logs), the router page's `apk add` block for the services and the computer page's pointer, this report, `packaging.md` 41 to 44. |

Where things are in the tree:

| | |
| --- | --- |
| `crates/monokulo-bell/` | the Bell: library and binary, `README.md`, `service/`, tests (`bell.rs`, `bell_over_http.rs`, `binary.rs`, `openwrt.rs`) |
| `crates/push-relay/` | the relay: library and binary `monokulo-push-relay`, `README.md`, `service/`, tests (`relay.rs`, `relay_over_http.rs`, `binary.rs`, `openwrt.rs`) |
| `crates/live-settings/` | `LoopbackAddr`, `cli::with_check`, `Env::with_renamed` |
| `crates/service-runtime/src/status.rs` | the status listener, behind the `status` feature |
| `openwrt/monokulo-push-relay/`, `openwrt/monokulo-bell/`, `openwrt/common/` | the packages and what they share |
| `scripts/build-openwrt.sh`, `scripts/test-openwrt.sh`, `scripts/check-service-files.sh`, `scripts/release-notes.sh` | the builds and their checks |
| `.github/workflows/release.yml` (`services`), `openwrt.yml`, `lint.yml` | CI |
| `tools/fake-square/`, `tools/fake-clover/` | the stand-ins the Bell's dev loop talks to |
| `xtask/src/pages/docs/bell.md`, `push-relay.md`; `xtask/src/pages/install/services.rs` | the docs site's `/bell/`, `/push-relay/`, `/services/` |
| `docs/bell.md` | the Bell's design note |
| `docs/decisions/bell.md`, `packaging.md`, `push.md` | the decision logs |

### Dependencies

No new crate. `crypto-bigint` moved to 0.7.10 (a security fix, under the
exception the owner's two-week rule makes for them; push.md 160, monokular
#21). The Bell's git dependencies on this repository's crates (`push`,
`live-settings`, `sqlite`, `rate-limit`, `abuse`, `service-runtime`,
`outbound`) became path dependencies; monokular's root
`[workspace.dependencies]` on them went with the Bell.

## 3. How to install it

The docs site has the steps: [Run the push relay and the
Bell](https://docs.monokulo.com/services/), linked from the router and
computer install pages, both LuCI pages and both READMEs. In brief:

**On an OpenWrt router.** Trust the Monokulo signing key and add the
repository as [Install on a router](https://docs.monokulo.com/install-router/)
says (once; the same feed as `monokulo`'s), then `apk update && apk add
monokulo-push-relay` or `apk add monokulo-bell`, or System › Software in
LuCI. Reload LuCI; the page is under Services › Push relay or Services ›
Bell. Set the public address and what it serves (the relay's apps with the
APNs key and the service account uploaded as files; the Bell's Square or
Clover application with its key or Auth Code set), Save & Apply: the page
runs the binary's `--check` and restarts the service only if it passes,
otherwise it shows the binary's words and the service keeps running on
what it had. Install Caddy (`apk add caddy`) with a three-line Caddyfile
proxying to `127.0.0.1:8444` or `:8445`, and open 80 and 443 for it; the
services' own firewall rules stay off unless the service itself must be
reached from the internet. Settings are `/etc/config/<name>`, secrets
`/etc/<name>/secrets/<VARIABLE>` (root's alone; back up the generated
one), data `/srv/<name>/`; a firmware upgrade and `apk del` both keep them.

**On a computer.** Download the zip for your computer from the latest
release, check its `.sha256`, unzip. Try it: `--check`, then run. Then the
service file for your system, as its header and the README say: Linux,
`install` the binary to `/usr/local/bin`, the options file to
`/etc/monokulo/`, the secrets as `VARIABLE='value'` lines in a root-only
`/etc/monokulo/<name>.env`, the unit to `/etc/systemd/system/`, `systemctl
enable --now`; macOS, the binary (quarantine cleared), the options file
and data folder under `/Library/Application Support/monokulo/`, the plist
(root's alone, the secrets in place of `REPLACE`) to
`/Library/LaunchDaemons/`, `launchctl bootstrap system`; Windows,
`powershell -ExecutionPolicy Bypass -File service\install-windows-task.ps1`
from an administrator PowerShell, which installs to `C:\Program Files\monokulo\<name>\`,
keeps options, secrets, data and log under `C:\ProgramData\monokulo\<name>\`
and registers a startup task as LOCAL SERVICE. Put Caddy or nginx in front
serving the public address.

## 4. The decisions, in brief

Each is a line here; the logs have the reasoning and the alternatives.

### The move (`docs/decisions/bell.md` 59 to 74)

59. The history is replayed, not merged: the five Bell commits rebuilt
    with only the Bell's paths, linearly on `main` (monokulo takes rebase
    merges only); `git filter-repo` couldn't be run in the session, so the
    same filter was done with git plumbing and the trees checked identical.
60. The moved paths are the crate, both stand-ins whole, the API doc and
    the decision log; monokular's report stays there and its design is
    copied by hand into `docs/bell.md`.
61. Authors and dates kept; messages rewritten only to name the monokular
    PR and source commit; the replayed commits aren't buildable states.
62. Paths renamed as they are replayed, so `git log --follow` works.
63. The Bell takes the workspace's version 0.1.0 (one version here).
64. It keeps edition 2024 (churn with no gain to change).
65. No `license` field: monokulo's crates have none and the repository no
    LICENSE; monokular's was `GPL-3.0-only`. Flagged in §6 below.
66. It logs through `telemetry`, with `logging.format`, so both standalone
    services write one log shape.
67. Setting keys stay `bell.*` and the file `bell.toml`; only the crate,
    binary, variables and `User-Agent` change name.
68. Secrets are `MONOKULO_BELL_*`; `MONOKULAR_BELL_*` read for one release
    through `Env::with_renamed`, a warning each, both set to different
    values stops it.
69. lint.yml drops the stable clippy of `push`/`outbound` (its consumer is
    here now), adds the Bell's production clippy and the boundary check.
70. The Docker image builds `monokulo-bell` too.
71. Problem types are `https://docs.monokulo.com/bell/#<slug>`; the till
    reads only the fragment.
72. "Run one" is a part of the `/bell/` page, as the relay's.
73. The stand-ins stay stdlib Python, run by no CI job.
74. The Bell has `--check` and a status listener; TLS stays its proxy's
    (packaging.md 1 to 12).

### `--check` and the status (`packaging.md` 1 to 12)

1. `--check` is the first half of a start (`Relay::check`, `Bell::check`),
   so there is one validation path.
2. It touches nothing on disk or the network, so root can run it for the
   service's user; a taken port is the start's to find.
3. Added by `live_settings::cli::with_check`, so the engine and monokulo
   don't offer a flag they'd ignore; conflicts with `--init`.
4. One output format: facts then `OK` on stdout, exit 0; the start's words
   on stderr, exit 1; no fact names a secret.
5. The status is a listener of its own, `status.listen`, typed
   `LoopbackAddr` (refuses a non-loopback address as it is parsed), never
   a route on the service's listener, which the proxy publishes.
6. It refuses a request whose `Host` isn't this machine (DNS rebinding).
7. Inside monokulo there is no relay status listener; the admin card is
   the status.
8. The status carries counts, times and readiness only; the last event is
   in memory.
9. Shared pieces live where their kind does: `LoopbackAddr` in
   `live-settings`, the listener in `service_runtime::status` behind a
   feature.
10. A status listener that can't bind stops the service, as the main one
    does.
11. `--version` names the commit when the build sets `MONOKULO_GIT_COMMIT`.
12. TLS is a proxy's job: neither binary serves it (checked: rustls reaches
    them only through `reqwest`, as clients); the clients trust the
    machine's certificate store, so `ca-bundle` on OpenWrt.

### Release zips and service files (`packaging.md` 13 to 20)

13. One `services` matrix job in release.yml, ten jobs, named as monokulo's
    zips; x86_64 macOS built natively on `macos-15-intel` so the service
    checks run on the computer the zip is for.
14. The zip's starter options file is the binary's own `--init` on the
    runner that built it.
15. Service files live in `crates/<crate>/service/` and go in the zip with
    the README; a unit test parses each with the binary's own parser and
    holds its variables to the binary's secrets.
16. systemd: `DynamicUser`, `StateDirectory`, options in `/etc/monokulo/`,
    secrets in a root-only `EnvironmentFile` (`VARIABLE='value'`, single
    quotes keep multi-line values), `ExecStartPre=… --check`, sandboxed.
17. launchd: a LaunchDaemon as `nobody`, secrets in the root-only plist,
    `REPLACE` placeholders the binary refuses.
18. Windows: a startup task as LOCAL SERVICE (the binaries speak no
    service control protocol), the script also the task's runner,
    program under Program Files, state under ProgramData, secrets one file
    per variable, the generated key made on install, `-Uninstall` keeps
    the data.
19. Every release runner installs and starts the service file from the
    folder it zipped and reads `/status` before the zip is kept.
20. Unsigned on macOS and Windows, as monokulo's zips are.

### OpenWrt packages (`packaging.md` 21 to 40)

21. One package per service with its LuCI page inside, as
    `openwrt/monokulo`; the shared half written once in `openwrt/common/`
    and installed as each package's own copy.
22. All three binaries in one cargo invocation and one SDK run, one signed
    feed; the site build fails if a feed lacks a package.
23. Built for `aarch64_cortex-a53` and `x86_64` (two matrix entries);
    x86_64 is what CI boots.
24. CI boots the OpenWrt 25.12 x86_64 rootfs (procd as PID 1) and runs
    `scripts/test-openwrt.sh`: install from the signed repository, the
    generated secret, the firewall rule off, refusals with their reasons,
    UCI to options file, the start as its own user, the status listener,
    LuCI's backend over uhttpd's JSON-RPC under the page's ACL alone, a
    secret set, refused and cleared, a bad value refused by `--check` on
    reload and restart, upgrade keeping settings and secrets, the relay
    refusing to run beside monokulo's, the sysupgrade list, removal.
25. A throwaway key signs a build with no real key, so a fork's PR builds a
    repository the rootfs test can verify.
26. Secrets are a root-only `/etc/<package>/secrets/`, one file per
    variable; the generated one is made in `postinst` and never replaced.
27. The options file is written from UCI at every start into memory, as
    dotted keys with every value a TOML string; only set options are
    written.
28. A bad setting never stops a running service and never starts a
    stopped one: each start renders, runs `--check`, and on failure writes
    the reason to `/tmp/<package>.check` and re-registers the running
    service on its last good file.
29. Save & Apply commits UCI, runs the backend's `check`, then reloads;
    refused, it shows the binary's output.
30. The page's backend is an rpcd ucode plugin (`status`, `check`,
    `set_secret`, `clear_secret`, `restart`, `reload`); the ACL grants no
    `service list`, whose instances hold the secrets.
31. The standalone relay refuses to start while monokulo on the same router
    has `relay.enabled`; the page's Status row says so.
32. Listen: the relay on the LAN by default, the Bell on this router only;
    the status listeners fixed at 8446 and 8447.
33. A firewall rule per package, `Allow-<package>`, made off; the packages
    install no proxy, the pages and docs give Caddy and nginx.
34. Removal keeps the data folder and the secrets.
35. The relay's apps are anonymous UCI sections with a `name` option.
36. A test in each crate holds its package to its settings
    (`tests/openwrt.rs`).
37. monokulo's own LuCI page no longer reads procd's `service list`, which
    carried the encryption key into the browser; it reads the package's
    backend.
38. Every refused start says why on the page, whatever refused it (QA A).
39. The refusal names the operator's settings, not the generated file (QA
    B).
40. The pages restart and reload only their own service; `setInitAction`
    is dropped from all three ACLs (QA C).

### The docs (`packaging.md` 41 to 44)

41. One `/services/` page for both services, drawn in Rust as the install
    pages are, linking the API pages' "Run one" rather than repeating them.
42. The router page gets one `apk add` block and a link; the computer page a
    sentence.
43. QA's D and E stay open, recorded in §7 with their fixes, rather than
    ride in the docs PR.
44. This report, shaped as `bell-and-relay.md`.

### The relay's cross-references (`push.md` 161 to 163)

161. CI no longer checks `push` and `outbound` on stable (the Bell is here
     now). 162. The relay's `--check` and status (packaging.md 1 to 12).
163. The relay's zip (packaging.md 13 to 20).

### Decisions made while merging

- Stage 1's rebased branch was pushed as `bell-into-monokulo-rebased` and
  the PR opened from it, because force-pushing the original branch was
  refused by the session's permission system; the original
  `bell-into-monokulo` (`210e6db18`) is stale and deletable.
- Windows CI was found red after #257 (the CRLF test) and fixed as its own
  PR (#260) rather than folded into stage 2, so each PR stays one change.
- Stage 3's Windows `ProgramData` fix stayed its own commit rather than
  being folded; the history is linear either way.
- #263's first run failed only the rootfs test's "monokulo's removal stops
  it": `apk del` returns before procd has ended the process. The check was
  split (`apk del`, eventually stopped, files gone) and the suite rerun
  locally, 99 of 99.
- Releases and the Pages deploy run on `main` after each merge; nobody
  waited for every main run to finish before the next stage merged, as the
  PR runs on identical content were green (and release.yml had been
  dispatched by hand on the stage 3 and 4 heads).

## 5. Tests and gates

### What holds each piece

| Piece | Held by |
| --- | --- |
| Monokulo never depends on the Bell | lint.yml: `cargo tree -p monokulo --all-features` holds no `monokulo-bell`, after checking the crate exists (tried both ways) |
| The Bell and the relay as production builds them | lint.yml: `cargo clippy -p <crate> --lib --bins` without `test-support` |
| `MONOKULAR_BELL_*` read for one release | `live-settings` tests of `Env::with_renamed` (unset new name read under old, warning per old name, both set differently is an error); the Bell's binary table runs each secret under its old name |
| `--check` says what a start would | both binary test tables (`tests/binary.rs`) run every refusal with and without `--check` and expect the same text; `--check` passes with both ports taken; nothing is written |
| The status | `tests/*_over_http.rs`: the JSON's fields, `null`s, the last event, a foreign `Host` refused (403), a non-loopback `status.listen` refused as a setting; monokulo refuses `[relay.status]` |
| `--version` with the commit | the binary tables, with and without `MONOKULO_GIT_COMMIT` |
| The service files | `cli.rs` `the_service_files_start_it_with_its_own_options_and_secrets`: the unit's `ExecStartPre`/`ExecStart`, the plist's `ProgramArguments` and the script's store option parsed by the binary's own parser; the variables held to the binary's secrets exactly |
| The zips | release.yml's ten `services` jobs: `--version`, `--init` on the runner, zip, `.sha256`; `scripts/check-service-files.sh` installs and starts the service file (systemd, launchd, the task as LOCAL SERVICE), reads `/status` until it names the service, stops and removes it, checks the status is gone |
| The OpenWrt packages against their binaries | `crates/<crate>/tests/openwrt.rs`: every setting the options file takes is written by the init script from a UCI option the LuCI page shows; the secrets of init, page and backend are the binary's; the default port and status listener are the binary's own |
| The packages on OpenWrt | openwrt.yml's `test` job: `scripts/test-openwrt.sh` in `openwrt/rootfs:x86-64-25.12.5`, 99 checks (decision 24's list, plus the three ACLs refusing `service list` and `setInitAction`, each refusal's reason on the page, and each service started through its own `restart`) |
| The docs pages | xtask's pages tests build the whole site into a scratch dir with a faked feed: every page written and in the map with its sections, the search index, the router page's feed lines and package files per repository, the computer page's release links, redirects |
| The problem types | the Bell's `every_problem_type_is_a_section_of_the_api_page` (line by line, CRLF-safe since #260) |

Counts at merge: `cargo nextest -p monokulo-bell -p push-relay -p xtask
-p live-settings -p service-runtime` 406 passed at stage 2, 324 for
`xtask`, `push-relay` and `monokulo-bell` at stage 4; the full workspace
suite (`--features monokulo/snp --exclude xtask`) 2559 at stage 2; the
rootfs suite 99 of 99; release.yml green through lint, Linux, macOS and
Windows tests, coverage and all ten services jobs on the stage 3 head
(run 40864370505) and all stage 4 content on run 40893777866 (both
builds and the rootfs test).

### What QA did by hand (stage 4, `~/.claude/jobs/dc2fa48b/tmp/packaging/qa.md`)

An operator's session in the rootfs with CI's signed x86_64 artifacts:
install without monokulo, UCI as LuCI writes it, both pages driven in a
real browser (Playwright Chromium against LuCI: APNs `.p8` uploaded
through the page, a bad `public_uri` refused in the relay's words with the
relay still running, Set/Clear of a secret, Restart), the page's ACL alone
refusing file reads, `service list` and another config; upgrade keeping
every secret's hash; removal keeping secrets, data and config;
reinstall not regenerating the key; `sysupgrade -l` listing everything;
the aarch64 packages' binaries static; stage 3's Linux zips run with
`--version`, `--init`, `--check` and `/status`. Verdict: fit to merge, five
non-blocking issues, A to C fixed before #263 merged, D and E open (§7).

### Not covered

- `aarch64_cortex-a53` is not booted (no rootfs image): its proof is the
  static-link check and the aarch64 packages' contents; the owner's Flint
  2 is the real test.
- The rootfs container has no `wan` interface, so the firewall rule's drop
  with the rule off isn't exercised; `fw4 check` accepts the rules.
- The init script's "waiting for the LAN address" refusal isn't exercised:
  the container's LAN always has one.
- The Windows task script is proven on GitHub's Windows runner only (no
  `pwsh` locally).
- Nothing was deployed to a real router or a real APNs/FCM/Square/Clover
  account by the team.

## 6. Licence

Decision bell.md 65: the Bell came from monokular, whose crates were
`GPL-3.0-only`; monokulo's crates have no `license` field and the
repository has no LICENSE file, so the moved crate has none either, as
every other crate here. The owner owns both repositories. If monokulo is
to carry a licence, add a LICENSE file and a `license` field to the
workspace's `[workspace.package]`, and the Bell takes it with the rest.

## 7. What is left

Open, in the order they matter:

1. **QA E, `--check`'s wording (push-relay, monokulo-bell, live-settings).**
   `LoadError`'s `#[error("the push relay's settings: {0}")]`
   (`crates/push-relay/src/settings.rs` 233; the Bell's at
   `crates/monokulo-bell/src/settings.rs` 218) wraps the registry's
   `BuildError::Invalid`, whose section problems already read `the push
   relay settings: …` (`crates/live-settings/src/registry.rs` 483), so the
   page shows `the push relay's settings: the push relay settings:
   handles.apps: …`, and with two errors only the first line has the outer
   prefix. One prefix per line: make `LoadError` carry the registry's
   problems as lines and drop its own prefix, or give the binaries' section
   problems no section prefix in a single-section registry. Also in
   `RelayConfig::from_snapshot` (settings.rs 428): an app with an
   `fcm_project_id` says `MONOKULO_RELAY_FCM_CREDENTIALS isn't set` when
   it is set but invalid (the `fcm()` helper returns `None` for both); say
   so only when the secret is unset, as the invalid value already has its
   own error, and give `check_fcm_credentials`' message a subject (`is set
   but isn't a service account's JSON key file`). The binary test tables
   hold the texts, so each change is one table line.
2. **QA D, the zip's starter options file names the runner's home.**
   `relay.toml` line 51 and `bell.toml` line 39 in every zip read `# path
   = "/home/runner/.local/share/monokulo/push-relay.db"` (Windows:
   `C:\Users\runneradmin\…`): `STORE_PATH`'s default is
   `paths::data_file(…)` evaluated on the runner. Commented out and
   described correctly on the line above, so nothing breaks, but the file
   states a wrong default. Either run `--init` in release.yml with a
   neutral `HOME` (and `LOCALAPPDATA` on Windows), or have `--init` show a
   home-relative default for a path under the home.
3. **The `MONOKULAR_BELL_*` names** (bell.md 68): remove
   `settings::RENAMED_ENV` and the docs' sentence in the first version tag
   after the one that ships them.
4. **The owner's router.** Install `monokulo-push-relay` or `monokulo-bell`
   on the Flint 2 from the feed once the Pages deploy after #263 has run,
   and read the page; the aarch64 build has not run anywhere.
5. **The docs site's deploy.** `/bell/` is on the site once release.yml's
   Pages job after #257 ran; `/services/` once this PR's does. Both LuCI
   pages link `/services/` already.
6. **Housekeeping.** Stale remote branches `bell-into-monokulo`
   (`210e6db18`, the un-rebased stage 1), `bell-into-monokulo-rebased`,
   `services-check-and-status`, `bell-problem-test-line-endings`,
   `release-service-zips` (all merged) can be deleted; the worktrees
   `~/Downloads/mokulo-wt-bell-move` and `~/Downloads/mokulo-wt-openwrt-services`
   belong to the engineers who made them.
7. **monokular, pre-existing.** `docs/design.md` §9 lists a `protocol/`
   directory that doesn't exist, and Kotlin comments under `android/` cite
   `docs/bell.md`, which is now
   `https://docs.monokulo.com/bell/`.
8. **Noted, not a defect.** Save & Apply on a LuCI page reloads the service
   twice (procd's `config.change` trigger, then the page's `reload`); the
   second finds the same options hash and restarts nothing.
