# Proposal: monokulo and the engine on a GL.iNet Flint 2, as one OpenWrt apk with a LuCI app

Status: proposal only. Nothing here is implemented. Written 2026-10-03 against
`b127291` (origin/main at the time).

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

`DEPENDS:=@aarch64 +ca-bundle +rpcd`. `ca-bundle` is required: reqwest uses
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
	option scan_threads '2'      # see "Scan CPU on a router"
```

No monokulo setting is mirrored into UCI. Mirroring would give each value
two homes, and the admin page and the options file are already the single
source for them.

### Keeping it through sysupgrade

`/lib/upgrade/keep.d/monokulo` lists `/etc/monokulo/` (secrets and options,
which are small). Whether to list `/srv/monokulo/` needs measuring on the
device. sysupgrade stages its backup in RAM, and the router has 1 GB, so a
large engine database could make the upgrade fail. If it is too big, the
LuCI app offers "Back up now" (the existing `scripts/backup-database.sh`,
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

The engine allows one scan per core at a time
(`engine/src/key_custody/plain.rs`, `SCAN_SLOTS`, sized by
`available_parallelism()`). On the Flint 2 that is all 4 cores during a
catch-up scan, which is exactly when the router also has to keep routing.
Two ways to deal with it, which can be combined:

- Run the engine at `nice 10` under procd. This needs no code change and is
  the first thing to try.
- Make the slot count a registered setting (`payment.scan_threads`, default
  = cores, so other deployments see no change), and have the package set it
  to 2. This is a small code change, so it is left for a decision here.

All of this needs measuring on the device: scan throughput per core on an
A53 compared with x86, and RSS for both processes idle and during catch-up.
The budget to aim for is under 150 MB combined.

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
| Optional: `payment.scan_threads` setting | small |
| Optional: SOCKS proxy for the engine's node client | small to medium |
| Optional: own tor build with PoW | small, plus maintaining it |

## Sources

- OpenWrt Table of Hardware, GL.iNet GL-MT6000: https://openwrt.org/toh/gl.inet/gl-mt6000
- OpenWrt 25.12.0 release (apk replaces opkg): https://linuxiac.com/openwrt-25-12-released-with-apk-package-manager-replacing-opkg/
- GL.iNet firmware lines for the Flint 2 (stock = 21.02, op24): https://forum.gl-inet.com/t/mt-6000-flint-2-firmware-versions/57139
- OpenWrt packages, `lang/rust` (1.96.0) and `rust-package.mk`, branch openwrt-25.12: https://github.com/openwrt/packages/tree/openwrt-25.12/lang/rust
- OpenWrt packages, `net/tor` (0.4.9.11), branch openwrt-25.12: https://github.com/openwrt/packages/tree/openwrt-25.12/net/tor
- LuCI app layout (`luci-app-ttyd`), branch openwrt-25.12: https://github.com/openwrt/luci/tree/openwrt-25.12/applications/luci-app-ttyd
