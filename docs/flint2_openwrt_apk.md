# Proposal: monokulo and the engine on a GL.iNet Flint 2, as one OpenWrt apk with a LuCI app

Status: the package, its LuCI page, the CI build and the landing page are
implemented (see "As built" at the end). The rest of this document is the
proposal they came from, written 2026-10-03 against `b127291`.

## Summary

It is feasible, and most of the hard part already works without code changes.

- Both binaries (`monokulo` and `monokulo-engine`, with the `zmq` feature)
  cross-compile today for `aarch64-unknown-linux-musl` with the OpenWrt
  25.12.5 mediatek/filogic SDK toolchain. They come out as fully static ELF
  files with no build warnings. Stripped, they are 17.7 MB and 14.5 MB.
- The SDK packs those prebuilt binaries into a valid OpenWrt 25.12 `.apk`
  (14.6 MB, `depends: libc ca-bundle`) with an ordinary package Makefile.
- The Flint 2 has plenty of room for it: 4 × Cortex-A53 at 2.0 GHz, 1 GB RAM,
  about 7.2 GB of writable overlay on eMMC.
- DESIGN.md already names this as the target ("a single static executable … must
  build for `*-unknown-linux-musl` and run on modest ARM or x86 router-class
  hardware"), and the defaults are already modest: 2 engine worker threads,
  an 8 MB scan chunk budget, loopback-only listeners.

The work left is packaging and integration: a procd init script, a UCI config
for the few options that must be fixed at launch, secret generation on first
start, keeping data across sysupgrade, and a LuCI app. Four product gaps
also need deciding: where the Monero node lives, Tor PoW on OpenWrt's tor
build, scan CPU use on a router, and how to reach the store from outside.

## Target platform

| | |
|---|---|
| Device | GL.iNet GL-MT6000 (Flint 2), MediaTek Filogic 830 (MT7986) |
| CPU | 4 × Arm Cortex-A53, 2.0 GHz, aarch64 |
| RAM | 1 GB DDR4 |
| Storage | 8 GB eMMC; OpenWrt puts the overlay on f2fs, about 7.2 GB usable |
| Firmware | vanilla OpenWrt 25.12.x (the owner has already flashed it) |
| OpenWrt target | `mediatek/filogic`, package arch `aarch64_cortex-a53`, musl libc |
| Package manager | `apk` (Alpine Package Keeper), which replaced opkg in 25.12 |

GL.iNet's own firmware is not a target. Its stock build is based on OpenWrt
21.02 and its "op24" build on an early 24.x snapshot, and both still use opkg
and `.ipk`. An `.apk` will not install on them. If GL.iNet firmware ever
matters, an `.ipk` built from the same binaries with a 24.10 SDK would be a
separate deliverable.

## What the probe showed

Everything below was run in scratch space outside the repository. No code in
the repository was changed.

1. Extracted the toolchain from the local `openwrt/sdk:mediatek-filogic-25.12.5`
   image (gcc 14.3.0, musl 1.2.5).
2. Built with the host's nightly Rust (1.101.0-nightly, 2026-10-01) and the
   `aarch64-unknown-linux-musl` target, with the SDK's gcc as the C compiler
   (for SQLite and aws-lc-sys) and as the linker:

   ```sh
   export STAGING_DIR=.../owrt-staging
   export PATH=.../toolchain-aarch64_cortex-a53_gcc-14.3.0_musl/bin:$PATH
   export CC_aarch64_unknown_linux_musl=aarch64-openwrt-linux-musl-gcc
   export CXX_aarch64_unknown_linux_musl=aarch64-openwrt-linux-musl-g++
   export AR_aarch64_unknown_linux_musl=aarch64-openwrt-linux-musl-ar
   export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-openwrt-linux-musl-gcc
   cargo build --release --locked --target aarch64-unknown-linux-musl \
     --features engine/zmq -p engine --bin monokulo-engine -p monokulo --bin monokulo
   ```

   It finished in 1 min 19 s on a 32-core machine, with no warnings.
3. Results:

   | Binary | Unstripped | Stripped | gzip -9 |
   |---|---|---|---|
   | `monokulo` | 25.1 MB | 17.7 MB | 7.6 MB |
   | `monokulo-engine` | 20.7 MB | 14.5 MB | 6.3 MB |

   `file` reports both as `ELF 64-bit LSB executable, ARM aarch64, statically linked`.
4. Packed both into a package with a minimal SDK Makefile (prebuilt binaries,
   `DEPENDS:=@aarch64 +ca-bundle`). `make package/monokulo/compile` produced
   `monokulo-0.1.0-r1.apk`, 14.6 MB.

Not tested: running the binaries. There is no aarch64 emulator on the build
host, and registering one through binfmt would change the host's system
configuration. The first thing to do on real hardware is run both binaries
with `--help`, then start them (see "Validation plan" below).

### Native code in the build

Only two C libraries are in the normal dependency tree, and both built cleanly
with the OpenWrt gcc:

- `libsqlite3-sys` (rusqlite's `bundled` feature).
- `aws-lc-sys`, which reqwest 0.13's rustls feature pulls in through
  `aws-lc-rs`. It is the one most likely to break on a toolchain change. If it
  ever does, rustls can be switched to the `ring` provider; `ring` is already
  in `Cargo.lock` through `snp-attest`.

`ring` and `x509-parser` come only from `snp-attest`, which neither binary uses.

### Toolchain choice

OpenWrt's packages feed ships Rust 1.96.0 (`lang/rust`, a host-only build) and
`rust-package.mk`, which builds with `cargo install` inside the SDK. That route
is a poor fit:

- The project builds on unpinned nightly (`rust-toolchain.toml`). The highest
  `rust-version` among our dependencies is 1.95 (`sysinfo`), so 1.96 probably
  works today, but nothing keeps it working.
- `crates/monokulo/build.rs` builds the POS app with Node 24 and Vite. The
  SDK has no Node, and building `lang/node` for the host would add a very
  long build for no gain.

**Recommendation:** build the binaries in our own CI with our toolchain and
the OpenWrt SDK's gcc, as the probe did, then have the SDK package them as
prebuilt files. That is two steps, but each one uses the tool built for it.

## Package design: one apk

Name `monokulo`, arch `aarch64_cortex-a53`. One file holds everything the user
asked for: both services and the LuCI app.

```
/usr/bin/monokulo
/usr/bin/monokulo-engine
/etc/init.d/monokulo                    procd script; two instances, engine and monokulo
/etc/config/monokulo                    UCI: only the options fixed at launch (see below)
/usr/share/monokulo/torrc.snippet       deploy/tor/torrc.snippet, for users who add tor
/lib/upgrade/keep.d/monokulo            paths sysupgrade keeps
/www/luci-static/resources/view/monokulo/*.js
/usr/share/luci/menu.d/luci-app-monokulo.json
/usr/share/rpcd/acl.d/luci-app-monokulo.json
/usr/libexec/rpcd/luci.monokulo         small rpcd plugin (status, secret rotation)
```

`DEPENDS:=@aarch64 +ca-bundle +rpcd +taskset`. `taskset` pins the engine to
two cores (see "CPU limits and tenant capacity"). `ca-bundle` is required: reqwest uses
`rustls-platform-verifier`, which reads the system CA store at
`/etc/ssl/certs`, for TLS to nodes and rate providers. Time zones need nothing
on the router because `jiff` has the database built in (`tzdb-bundle-always`).

The LuCI files cost a few kilobytes and do nothing on a router without LuCI,
so there is no hard `luci-base` dependency. The OpenWrt convention is a
separate `luci-app-monokulo` package. If we ever submit to the official
feeds we should split it then, with the same files. Until then, one apk is
what was asked for.

The package creates a `monokulo` system user (`USERID:=monokulo=…:monokulo=…`;
the SDK already generates the `.rusers` hook for this, as the probe log
showed). Both processes run as that user under procd, never as root.

### Where data lives

On OpenWrt, `/var` is a symlink into `/tmp`, which is RAM. Databases must
not go there. The proposed layout:

| Path | Contents | Mode |
|---|---|---|
| `/etc/monokulo/secrets` | `MONOKULO_ENCRYPTION_KEY`, `ENGINE_TOKEN` | 0600 root |
| `/etc/monokulo/monokulo.toml`, `engine.toml` | options files the admin page saves to | 0640 monokulo |
| `/srv/monokulo/monokulo.db`, `engine.db`, `*.logs.db` | databases and log stores | 0750 monokulo |

`/srv` is on the overlay, so it survives a reboot. SQLite runs in WAL mode
(`shared/src/sqlite.rs`), which is gentle on flash. `logging.max_mb` and
`logging.retention_days` already limit the log stores. The package should set
lower defaults through the options file it ships, not with a code change.

### The procd service

There is one init script with two procd instances. That way the LuCI
Start/Stop button and `service monokulo …` act on both together.

- **`engine`**: `monokulo-engine --options /etc/monokulo/engine.toml
  --server-bind 127.0.0.1:8443 --database-path /srv/monokulo/engine.db`,
  with `ENGINE_TOKEN` from the secrets file passed through `procd_set_param env`.
  The command is prefixed with `taskset -c <engine_cpus> nice -n <engine_nice>`
  (2,3 and 10 by default). Both commands exec the next one, so procd still
  tracks the engine's own PID. The jail must include the `taskset` binary,
  or the jail can be applied after the pinning.
- **`monokulo`**: `monokulo --options /etc/monokulo/monokulo.toml
  --server-bind <lan or 0.0.0.0>:8081 --database-path /srv/monokulo/monokulo.db
  --engine-url http://127.0.0.1:8443`, with `MONOKULO_ENCRYPTION_KEY` and
  `MONOKULO_ENGINE_TOKEN` from the same file.
- Both get `respawn`, `user monokulo`, `stdout 1`/`stderr 1` (so logs land in
  `logread`), and `procd_add_jail` with access to only `/srv/monokulo`,
  `/etc/monokulo` and `/etc/ssl`. The jail is equivalent to what the SEV-SNP
  systemd unit does with `ProtectSystem`/`ReadWritePaths`.
- `shared/src/shutdown.rs` already handles SIGTERM, which is what procd sends
  on stop.
- On first start, if `/etc/monokulo/secrets` is missing, the init script
  writes two keys from `/dev/urandom`: `hexdump -vn32 -e '32/1 "%02x"'`,
  64 hex characters each, which meets the engine token's 32-character minimum.
  It never overwrites an existing file. Losing the encryption key loses the
  data, so LuCI must show a "back this up" warning.

This matches the settings rules already in place. Options given on the
command line show as locked on monokulo's admin page. Every other setting
stays in the options file and the database, which the admin page edits.
Secrets come only from the environment.

### UCI: launch options only

`/etc/config/monokulo` holds only what has to be fixed when a process
starts, plus the things that belong to the router rather than to monokulo:

```
config monokulo 'main'
	option enabled '1'
	option listen 'lan'          # lan | all | loopback
	option port '8081'
	option open_wan '0'          # add an fw4 rule for the port on wan
	option data_dir '/srv/monokulo'
	option engine_port '8443'    # always on 127.0.0.1
	option engine_cpus '2,3'     # taskset list for the engine; see "CPU limits and tenant capacity"
	option engine_nice '10'
```

No monokulo setting is mirrored into UCI. Mirroring would give each value
two homes, and the admin page and the options file are already the single
source for them.

### Keeping it through sysupgrade

`/lib/upgrade/keep.d/monokulo` lists `/etc/monokulo/` (secrets and options,
which are small). Whether to list `/srv/monokulo/` needs measuring on the
device. sysupgrade stages its backup in RAM, and the router has 1 GB, so a
large engine database could make the upgrade fail. If it is too big, the
LuCI app offers "Back up now" (the existing `deploy/backup/monokulo-backup.sh`,
via SQLite's online backup) to a USB disk or a download.

The 25.12 tools that rebuild the firmware (Attended Sysupgrade, `owut`) build
images only from the official feeds, so they leave our package out. After an
upgrade, `apk add monokulo` brings it back, and the data is still there. The
LuCI page should point this out.

## The LuCI app

LuCI on 25.12 renders pages in the browser: JavaScript views in
`/www/luci-static/resources/view/`, a menu entry in `menu.d`, and an rpcd ACL
that limits what a view can read and call. `luci-app-ttyd` is a good model to
copy. The app's job is to run the service on the router. Monokulo's own admin
page remains the place for monokulo settings.

Menu: **Services → Monokulo**, with three tabs.

1. **Overview**
   - Running or stopped for each instance, from procd (`service list` over
     ubus), with Start, Stop and Restart buttons.
   - Health from monokulo's public `GET /status/summary`, fetched through the
     rpcd plugin on the router. The browser never reaches the engine, and
     LuCI never talks to it (the engine is private: admin API and `/status`
     for monokulo only).
   - An "Open monokulo admin" link to `http://<router>:<port>/`.
   - Disk use of the data directory, and memory use of each process
     (`/proc/<pid>/status`).
2. **Settings**: a UCI form for the options above. Saving restarts the
   service, as LuCI's apply step normally does. An `open_wan` checkbox
   writes or removes one fw4 rule and explains what that exposes.
3. **Logs**: the last N lines of `logread -e monokulo`, with a Refresh
   button. The full structured log viewer stays in monokulo's admin.

Plus two actions on Overview, both behind confirmation dialogs:

- **Download a backup** of `/etc/monokulo` and a consistent database snapshot.
- **Rotate the engine token**: write a new one, then restart both instances.
  The encryption key cannot be rotated without re-encrypting the data, so
  there is no button for it.

The rpcd ACL grants read/write on UCI `monokulo` and calls to the
`luci.monokulo` plugin, and nothing more. In particular, it gives no file
access to `/etc/monokulo/secrets`.

## Gaps to decide before building

### 1. Where the Monero node lives (biggest)

A Flint 2 cannot run `monerod` as a useful node. Even a pruned chain is well
over 7 GB, and syncing on four A53 cores with 1 GB of RAM is impractical. The
engine needs a node it can reach:

- **A node on the LAN** (a NAS or desktop). This is the best option, and with
  `--zmq-pub` the `zmq` feature gives fast mempool detection.
- **A remote node over HTTPS.** This works today. The engine scans with the
  view key locally, so the node never sees keys. It does see the router's IP
  and polling once a second (`payment.mempool_poll_interval_ms`). For a
  remote node we should suggest 2 to 5 s.
- **A remote node over Tor.** The engine has no SOCKS proxy setting today, so
  `.onion` nodes are not reachable. Adding one (reqwest supports SOCKS5) is the
  only code change this proposal would ask for, and it is optional.

### 2. Tor's proof-of-work module

`docs/TOR.md` needs tor 0.4.8 or newer with `pow: yes`. OpenWrt 25.12 packages
tor 0.4.9.11, but its Makefile does not pass `--enable-gpl`, and tor builds
the PoW module only with that flag. So the stock `tor` package probably reports
`pow: no`, and tor would refuse our snippet's `HiddenServicePoWDefensesEnabled 1`.
Check with `tor --list-modules` on the router. If PoW is missing, there are
two choices: ship a snippet without the PoW lines for OpenWrt and rely on
monokulo's own abuse protection and the intro-point limits, or build our own
tor package with `--enable-gpl`. We should not make the package depend on tor.

### 3. Scan CPU on a router

Recommendation: limit the engine to 2 of the 4 cores, run it at `nice 10`,
and do both from the init script. This needs no code change. The cost is
slower catch-up after downtime, on the order of minutes. When the engine is
up to date, the router won't notice it. The detail is in
"CPU limits and tenant capacity" below.

## CPU limits and tenant capacity

### What a scan costs

The engine checks each transaction against each store
(`engine/src/key_custody/outputs.rs`, `pays`). With view tags, nearly all of
that cost is one shared-secret derivation (8vR: a point decompression, a
variable-base scalar multiplication and a compression) per transaction per
store. Each output then adds a hash, which is cheap.

A scratch benchmark of exactly those calls (`monero` 0.22, one tx key and
two tagged outputs per transaction, one pinned core) measured:

| CPU | Backend | Per store per transaction | Per core |
|---|---|---|---|
| Ryzen 9 5950X | curve25519-dalek AVX2 (picked at run time) | 36 µs | ~27,000/s |
| Ryzen 9 5950X | curve25519-dalek serial (u64), as aarch64 uses | 51 µs | ~19,500/s |
| Cortex-A53, 2.0 GHz (Flint 2) | serial | **~300 to 700 µs, estimated** | **~1,500 to 3,300/s** |

The A53 figure is an estimate, not a measurement. Curve25519 in generic C
takes about 550k cycles on a Cortex-A53 (SUPERCOP, Raspberry Pi 3), roughly
3 to 4 times what the same code takes per clock on a modern x86 core, and the
Flint 2 clocks at 2.0 GHz against the 5950X's ~4.9 GHz. Together that makes
it 6 to 14 times slower. The rest of this section plans on **~450 µs, about
2,200 per second per core**. The same benchmark, built for the router
(`scanbench-aarch64`, 0.5 MB, static), replaces the estimate with a real
number in a few seconds.

### What the engine actually scans

Three facts from the code keep the load small:

- **Stores with nothing in scope cost nothing.** A store with no open order,
  and none closed within `payment.expired_order_grace_period_minutes`
  (6 h by default), is not scanned at all. Its cursor jumps past each block
  in the same transaction that records the block (`advance_idle_cursors`).
  A store only costs CPU while it has an order in flight.
- **A pool transaction is scanned once per store** (`work/mempool.rs`), not
  once per poll. Its block scans it again later, so each transaction costs
  about two derivations per active store.
- **Idle polling is light.** When no store has anything in scope, the
  engine doesn't even ask the node for the pool.

Mainnet carries about 27,000 to 30,000 transactions a day in 2026, about
0.35 a second or 42 per block. So with **A** stores that have orders in flight:

| Situation | CPU needed | With A = 5 | With A = 20 |
|---|---|---|---|
| Steady state (pool + blocks) | 0.35 × 2 × A × 450 µs per s | 0.16 % of one core | 0.6 % of one core |
| One block arriving | 42 × A × 450 µs | 95 ms, every 2 min | 0.38 s, every 2 min |
| A spam wave at 5× normal volume | 5 × steady | 0.8 % of one core | 3 % of one core |
| Catching up 1 day (after downtime) | 30,000 × A × 450 µs | 68 s of CPU | 270 s of CPU |
| Catching up 1 week | 7 × the above | 8 min of CPU | 32 min of CPU |

Fixed costs come on top: the node polls (once a second while anything is in
scope), the HTTP and JSON handling, and SQLite writes. They are not measured
on an A53 yet. On x86 they are lost in the noise, and they don't grow with
the number of stores.

### What limiting to 2 cores does

**For the router: almost nothing in steady state, and protection during
catch-up.**

- Day to day the engine needs well under 1 % of one core, so whether it has
  2 cores or 4 changes nothing, for the router or for the engine.
- During catch-up, uncapped, the engine would run 4 scans at once
  (`SCAN_SLOTS` = `available_parallelism()`) and could keep all 4 cores busy
  for minutes. Capped at 2, it can never take more than half the CPU. The
  other 2 cores stay free for the kernel's packet processing and for
  hostapd, dnsmasq, WireGuard and LuCI.
- How much the router needs those cores depends on its configuration. With
  hardware flow offloading on (the MT7986's PPE, plus WED for Wi-Fi), routed
  and NATed traffic mostly bypasses the CPU, and even 4 busy cores would
  barely show. What really uses the CPU on this SoC is SQM/cake,
  WireGuard/OpenVPN, software NAT without offload, and DNS filtering. Those
  are the setups that need the 2 free cores.
- `nice 10` matters as much as the cap. It gives each engine thread about a
  tenth of the weight of a normal task, so anything else that wants the CPU
  gets it first, including the softirq work that spills into `ksoftirqd`.
  The cap limits how much CPU the engine can take; nice decides who wins when
  both want the same core.

**For the engine: catch-up takes about twice as long, and nothing else changes.**

- One day of backlog for 5 active stores: about 34 s of wall time on 2 cores
  instead of 17 s. One week: about 4 min instead of 2. Downloading the
  backlog (roughly 30 to 60 MB of pruned blocks a day) from a remote node can
  easily take longer than the CPU work.
- Zero-confirmation detection while catching up: the fast pool pass waits for
  the same scan slots, but a batch is 32 transactions (`txs_per_scan`), about
  15 ms on an A53. A new payment waits at most one batch for a slot.
- Under load, the scheduler's 10 s round still splits the time between tiers
  (`ScanTuning::DEFAULT`: blocks 40 %, settlement 20 %, chain 20 %, pool 15 %,
  upkeep 5 %), so settlement and webhooks keep running during catch-up.

**How, without a code change.** Rust's `available_parallelism()` counts the
CPUs the process is allowed to run on (its affinity mask) and respects a
cgroup v2 `cpu.max` quota. Starting the engine as
`taskset -c 2,3 nice -n 10 monokulo-engine …` makes `SCAN_SLOTS` 2, and caps
the Tokio blocking pool and workers at those cores too. `taskset` comes from
OpenWrt's `taskset` package (util-linux), which becomes a package dependency.
Cores 2 and 3 are suggested because CPU 0 usually takes the most interrupts.
Check `/proc/interrupts` on the device, and leave the choice in UCI.
Monokulo itself does little CPU work: page rendering, and Argon2id at sign-in
(19 MiB, about 0.2 to 0.3 s on an A53 for each sign-in). It can stay
unpinned, at the default priority.

A `payment.scan_threads` setting is not needed for this. Add it only if a
deployment wants fewer scan slots than cores without pinning.

### A handful of stores: yes, easily

For roughly 5 to 20 stores the Flint 2 is not the limit:

- **CPU**: see the table above. Even with 20 stores active at once, the
  steady state stays under 1 % of one core. The only noticeable cost is
  catch-up after the router has been off, and even then a day's backlog for
  20 active stores is about 2 min on 2 cores.
- **Memory**: from scratch with no stores (x86 release build, pinned to 2
  CPUs, 10 threads each), the engine used 18.6 MB RSS and monokulo 22.2 MB.
  Each store adds a table of its subaddresses in scope (32-byte keys) and
  some rows. On top of that come the scan memory budget (8 MB per network by
  default), the pool bodies, and monokulo's HTTP cache. A realistic total is
  well under 150 MB of the router's 1 GB. Measure on the device to confirm.
  (Added after this was written: proof-of-work checking, on by default for
  mainnet, holds a 256 MiB RandomX cache on top of that; see "As built".)
- **Storage**: orders, payments and webhooks grow slowly. The log stores are
  capped by `logging.max_mb`. Several years of a handful of small stores'
  orders fit in hundreds of MB, not GB.
- **The node**: all stores share one node connection and one block cache, so
  more stores don't mean more downloads.

When the stores are multiple merchants rather than one owner, other limits
matter before the hardware does:

- **Availability is shared.** One home connection and one router serve every
  store's checkout. A reboot or an ISP outage stops all of them. Payments
  sent meanwhile are still found later, but customers see a dead checkout.
- **Trust.** Each store's view key lives on the router, encrypted at rest
  with a key that is also on the router. Whoever controls the router can see
  every store's incoming payments. Nobody can spend them, since every wallet
  is watch-only. Merchants need to accept that.
- **Exposure.** Public checkouts mean a public endpoint (gap 4), and abuse
  protection is per store and per client. A busy or attacked store competes
  with the others for one uplink.

Signup should stay in invite mode (`signup.mode`), so the operator
chooses who gets a store.

### To measure on the device

1. `./scanbench-aarch64 20000`: microseconds per store per transaction on
   the A53. Divide the tables above by (that ÷ 450 µs).
2. Catch-up with 5 stores that have open orders, from a height one day
   back: wall time, the engine's CPU (`top`), and routing throughput and
   latency (`iperf3` through the router, with SQM on) at the same time.
   Run it once with `taskset -c 2,3 nice -n 10` and once without.
3. RSS for both processes, idle and during that catch-up.

### 4. Reaching the store from outside

A home router is often behind CGNAT, and a static site's checkout needs a
public URL (`public_url`). The options are an onion service (gap 2), a port
forward with `open_wan` and a DNS name, or a reverse tunnel. The proposal
does not choose one. The LuCI Settings tab should state that exposing the
port means monokulo's abuse protection is the only defence in front of it.

## Build and release flow

1. In CI (new job, Linux x86-64): pull `openwrt/sdk:mediatek-filogic-25.12.x`,
   take its toolchain, `npm ci` the POS app, and build both binaries for
   `aarch64-unknown-linux-musl` as in the probe.
2. In the same SDK container, `make package/monokulo/compile` with the binaries
   copied into `files/`. This produces `monokulo-<ver>-r<n>.apk`.
3. Sign the feed (apk uses an ECDSA key; tools like `owfeed` produce a signed
   index), publish it, and have users add the public key to `/etc/apk/keys/`
   and the feed to `/etc/apk/repositories.d/`. Until a feed exists:
   `apk add --allow-untrusted ./monokulo-*.apk`.
4. Pin the SDK version per OpenWrt release, because packages target one
   release series.

Size options, all optional: `[profile.release] strip = true`, `lto = "fat"`,
`codegen-units = 1`, `opt-level = "s"`. `panic = "abort"` is ruled out:
`shared/src/supervise.rs` and the SQLite pools catch panics to restart a
failed task, and abort would take the whole process down instead. A
multi-call binary (one file, choosing engine or monokulo by `argv[0]`)
would share tokio, axum, rustls and SQLite between the two and save an
estimated 8 to 10 MB. It would blur the two-process boundary, though, so it
is not recommended unless space becomes a problem. It is not one today.

## Validation plan (first steps on the device)

1. `apk add --allow-untrusted monokulo-*.apk`. Check that the user, the
   secrets file, the services and the LuCI menu all exist.
2. `monokulo --help` and `monokulo-engine --help` (confirms the binaries run
   on the A53 and musl).
3. Start the services, sign up as admin, add a stagenet node on the LAN, and
   take a stagenet payment end to end.
4. Measure: RSS idle and during catch-up, CPU during catch-up with and
   without `nice`, routing throughput while scanning, and database growth
   per 1,000 orders.
5. `tor --list-modules` for the PoW question.
6. Run a sysupgrade with the package installed. Confirm that secrets and
   data survive and that reinstalling the package brings the service back.

## Effort estimate

| Item | Size |
|---|---|
| CI job: cross-build + SDK packaging | small (the probe already did it) |
| Package Makefile, init script, UCI defaults, keep.d, first-start secrets | small |
| LuCI app: 3 views, rpcd plugin, ACL, menu | medium |
| On-device validation and tuning of defaults | medium (needs the router) |
| Engine CPU pinning and nice in the init script (no engine change) | small |
| Optional: SOCKS proxy for the engine's node client | small to medium |
| Optional: own tor build with PoW | small, plus maintaining it |

## As built

The package was first built with two binaries and two procd instances (PR
#36). Once the engine could run inside monokulo (`docs/engine_as_library.md`,
phase 5) it became one binary and one instance; this section describes that.

The files:

| Path | What it is |
|---|---|
| `scripts/build-openwrt.sh` | Builds everything: copies the SDK's cross toolchain out of its image, cross-compiles monokulo (the engine built into it) with it, has the SDK package and sign them, and renders the landing page. Output in `dist/` and `site/`. |
| `openwrt/monokulo/Makefile` | The package: the binary, the init script, the UCI config, the keep list and the LuCI files. |
| `openwrt/monokulo/files/monokulo.init` | procd service with one instance, `monokulo`, the engine inside it. |
| `openwrt/monokulo/files/monokulo.config` | `/etc/config/monokulo`. |
| `openwrt/monokulo/files/luci/` | The LuCI view (Services › Monokulo), its menu entry and its ACL. |
| `openwrt/keys/monokulo.pem` | The public signing key. It is the same key as kringle's. |
| `web/index.html` | The landing page template, with install steps. |
| `.github/workflows/openwrt.yml` | Builds on every PR and push to main, uploads the artifacts, and deploys Pages from main. |

### Decisions

Each line gives what was decided, the alternatives, and why.

- **One package, not `monokulo` plus `luci-app-monokulo`** (kringle uses two).
  The brief asked for a single APK and the proposal recommended one. The
  LuCI files are three small files that do nothing without LuCI, and one
  package means one thing to install, upgrade and remove. The package
  installs them with plain `INSTALL_DATA` rather than `luci.mk`, so the
  build needs no LuCI feed and no network access in the SDK.
- **C code compiled with the SDK's gcc, Rust with our own nightly.**
  aws-lc-sys and SQLite need a C cross compiler. Kringle gets by with
  `rust-lld` because it is pure Rust. The toolchain is copied out of the
  same SDK image that does the packaging, and cached in
  `target/openwrt-sdk/<tag>`, so nothing else has to be installed.
- **No `panic=immediate-abort` or `build-std`** (kringle uses both to save
  size). `shared::supervise` and the SQLite pools catch panics to restart a
  task, and abort would take the whole process down instead. The binaries
  are only stripped (`CARGO_PROFILE_RELEASE_STRIP=symbols`).
- **The package version is the crate version, and the release number is the
  commit count** (`0.1.0-r<N>`). Every build from main is then an upgrade
  over the one before, without bumping the crate version. CI checks out the
  full history to count it.
- **Secrets are made by the init script on first start**, written to
  `/etc/monokulo/secrets` (root, 0600) and never overwritten. If the file is
  damaged, the service refuses to start rather than making new secrets.
  The alternatives were making them in the package's postinst (but then a
  sysupgrade that restores the config would race it) or asking the user
  (an extra step that is easy to get wrong).
- **Listen on the LAN address by default** (kringle defaults to all
  interfaces). A payment gateway's admin login shouldn't be on the WAN until
  the owner chooses that. The landing page explains how to open it, and the
  Tor setup.
- **One binary, one procd instance, the engine inside monokulo.** No engine
  port, no engine token (the secrets file holds only
  `MONOKULO_ENCRYPTION_KEY`), and no HTTP between the two. One options file,
  `monokulo.toml` in the data folder, holds the engine's settings under
  `[engine.*]`. The package is 10.5 MB, against about 15 MB with two
  binaries (the stripped binary is 24 MB), since tokio, axum, rustls and
  SQLite are linked once.
- **The engine's threads on CPUs 2 and 3, at nice 10, set by monokulo
  itself.** UCI's `engine_cpus` and `engine_nice` become
  `--engine-server-cpus` and `--engine-server-nice`, which the engine
  applies to each of its own threads (`engine::threads::ThreadPlan`), so
  monokulo's web threads keep normal priority on every core. The first build
  pinned the whole engine process with `taskset` through a wrapper script and
  procd's `nice`; with one process that would also have pinned and niced
  monokulo's web pages, so the wrapper is gone. monokulo refuses to start
  with CPUs that don't exist, so the init script still tries the list with
  BusyBox's `taskset` first and, if it doesn't fit the router, logs that and
  starts the engine on all CPUs rather than leaving monokulo unable to
  start.
- **RandomX (the engine's proof-of-work check, added on main after the
  proposal) is linked statically.** `randomx-rs` asks for `libstdc++` as a
  shared library, which made the binary a dynamic executable with glibc's
  loader path, one that can't run on OpenWrt. The build script now puts the
  toolchain's `libstdc++.a` alone in a search directory that is checked
  first, links `libgcc` statically for `__clear_cache`, and refuses to package
  a binary that `file` doesn't call statically linked. The alternative was
  patching `randomx-rs` (a `[patch]` or a fork), which has to be maintained.
- **Proof-of-work checking stays on for mainnet by default.** It costs 256 MiB
  (512 MiB for a moment around a key change) of the Flint 2's 1 GB. That
  fits, alongside about 40 MB for monokulo and OpenWrt itself. It
  is also what keeps a lying node from faking payments. The landing page
  says how to turn it off for someone who runs their own node.
- **Data in `/srv/monokulo` and everything kept across sysupgrade.** For a
  handful of stores the databases are small. A large deployment should
  measure the backup size against RAM first (see "Keeping it through
  sysupgrade").
- **The landing page uses monokulo's own `theme.css`**, which the build
  copies in, along with its fonts and logo. It has no colour of its own, so
  the page matches the app in light and dark.
- **Pages deploys only from main, and only after Pages is switched to
  GitHub Actions** in the repository's settings. Pull requests upload the
  site as an artifact to review instead.
- **No GitHub release step yet.** Version tags already make a release in
  `release.yml`. Attaching the `.apk` to it is a small follow-up, best done once
  versions are tagged.
- **Not done yet from the proposal:** the LuCI logs tab and backup
  download. (Token rotation is moot: there is no engine token any more.) The LuCI page shows how to back up the secrets with
  `scp`, and `logread -e monokulo` shows the logs.

## Sources

- OpenWrt Table of Hardware, GL.iNet GL-MT6000: https://openwrt.org/toh/gl.inet/gl-mt6000
- OpenWrt 25.12.0 release (apk replaces opkg): https://linuxiac.com/openwrt-25-12-released-with-apk-package-manager-replacing-opkg/
- GL.iNet firmware lines for the Flint 2 (stock = 21.02, op24): https://forum.gl-inet.com/t/mt-6000-flint-2-firmware-versions/57139
- OpenWrt packages, `lang/rust` (1.96.0) and `rust-package.mk`, branch openwrt-25.12: https://github.com/openwrt/packages/tree/openwrt-25.12/lang/rust
- OpenWrt packages, `net/tor` (0.4.9.11), branch openwrt-25.12: https://github.com/openwrt/packages/tree/openwrt-25.12/net/tor
- LuCI app layout (`luci-app-ttyd`), branch openwrt-25.12: https://github.com/openwrt/luci/tree/openwrt-25.12/applications/luci-app-ttyd
