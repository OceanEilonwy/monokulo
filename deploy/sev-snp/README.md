# Deploying MoneroPay Cloud on a bare-metal AMD SEV-SNP host

WBS 2.2: provisioning + attestation (2.2.1) and deploying 2.1's engine +
`key-custody-server` split inside the confidential VM (2.2.2).

This directory covers the actual **deployment** once you have a bare-metal
host with SEV-SNP support (2.2.1's own provisioning step - "provider
console/API provisioning" - is out of scope for this repo: it's a real
account/hardware decision, made bare-metal deliberately for hosting
flexibility given the crypto-adjacent nature of this business, rather than
locked into one hyperscaler's confidential-VM product). Once you have a box:

## 1. Verify the host is genuinely SEV-SNP-protected, with AMD-SB-3019 fixed

**Do this before deploying anything.** A box that merely has SEV-SNP-capable
hardware is not the same as one actually running a confidential VM with a
patched microcode - this step is what tells the difference, cryptographically,
not by trusting the provider's own claim.

1. Boot the guest VM (SEV-SNP is a *VM* isolation feature - even on
   bare-metal-provisioned hardware, "the box" here means the confidential VM
   running on it, not the bare host OS directly).
2. Inside the guest, obtain a real attestation report. This repo doesn't
   reimplement `/dev/sev-guest`'s `SNP_GET_REPORT` ioctl - use AMD's own
   reference tool, [`snpguest`](https://github.com/virtee/snpguest)
   (`cargo install snpguest`, or your distro's package if it has one):

   ```sh
   snpguest report /tmp/report.bin /tmp/request-data.txt --random
   ```

3. Verify it with this repo's own tool (`snp-attest`, WBS 2.2.1's literal
   deliverable - see `snp-attest/src/bin/verify-snp-attestation.rs` for the
   full design rationale, including exactly why the `--min-*-spl` flags are
   optional):

   ```sh
   verify-snp-attestation --report /tmp/report.bin --product Milan \
       --min-microcode-spl <N>
   ```

   `--product` must match your actual CPU generation (Milan/Genoa/Turin -
   `lscpu` or your provider's spec sheet tells you which). The
   `--min-microcode-spl` (and sibling `--min-bootloader-spl`/`--min-tee-spl`/
   `--min-snp-spl`) flags enforce a floor on the report's cryptographically
   verified TCB - **you supply the actual number**, sourced from
   [AMD-SB-3019](https://www.amd.com/en/resources/product-security/bulletin/amd-sb-3019.html)
   (the "StackWarp" SEV-SNP vulnerability, CVE-2025-29943, fixed by AMD's
   2025-07-29 microcode release - this is the "AMD's July 2025 microcode
   patch" this WBS item names) or from your specific provider/hardware
   vendor's own documentation of which SPL that release maps to for your
   silicon. This tool does not hardcode that number itself - see the
   binary's own module doc comment for why guessing it would be worse than
   not checking it at all. Running without those flags still gets you full
   cryptographic chain verification and the real, signed SPL values printed
   for you to compare by hand.
4. **Do not proceed to step 2 below if this fails.** A failure here means
   either the box isn't genuinely SEV-SNP-protected, or its microcode
   predates the fix this WBS item exists to confirm - in both cases,
   deploying onto it defeats the entire point of this track.

## 2. Install the binaries

Build `moneropay-core` and `key-custody-server` (this workspace's existing
release build - `cargo build --release --workspace` from the repo root) and
copy the two binaries onto the guest:

```
/opt/moneropay/bin/moneropay-core
/opt/moneropay/bin/key-custody-server
```

Create the `moneropay` system user/group and the directories the systemd
units below reference (`/etc/moneropay`, `/var/lib/moneropay`) if they don't
already exist:

```sh
useradd --system --no-create-home --shell /usr/sbin/nologin moneropay
mkdir -p /etc/moneropay /var/lib/moneropay
chown moneropay:moneropay /var/lib/moneropay
```

## 3. Configure

Write `/etc/moneropay/moneropay.toml` (see `docs/DESIGN.md` §13 for the full
configuration surface). The one section this deployment specifically needs,
beyond whatever a self-hosted install would already have:

```toml
[key_custody]
backend = "socket"
socket_path = "/run/moneropay/key-custody.sock"
```

This must exactly match the socket path both systemd units below reference.
Everything else in `moneropay.toml` (node RPC endpoints, `[server].bind`,
etc.) is identical to a plain self-hosted deployment - nothing about running
inside a confidential VM changes any other config surface, per WBS 2.1.3's
own outcome ("swap which `KeyCustody` implementation the engine constructs...
behind a config flag").

## 4. Install and start the two units

```sh
cp deploy/sev-snp/moneropay-key-custody.service deploy/sev-snp/moneropay-engine.service \
   /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now moneropay-key-custody.service
systemctl enable --now moneropay-engine.service
```

Order matters for the *first* start only (start key-custody first so the
engine's own bounded reconnect loop - see `main.rs`'s
`connect_socket_key_custody` - has something to connect to right away rather
than burning its ~5s retry budget); `enable`d, both come up in the right
order on every subsequent boot regardless, since the engine unit's own
`Restart=on-failure` plus that reconnect loop tolerate the ordinary boot-time
race either way.

## 5. Confirm it's actually alive, then re-run 2.1.3's regression suite

Per this WBS item's own test: "re-run 2.1.3's regression suite against the
deployed instance over the network." Concretely:

1. `systemctl status moneropay-key-custody moneropay-engine` - both `active
   (running)`, not just `enabled`.
2. `journalctl -u moneropay-engine -n 50` - look for the real successful
   startup path: config loaded, `[key_custody] backend = "socket"` dispatch
   confirmed, a scan-loop tick logged shortly after start (a process that's
   up but never ticks is not actually working - same standard
   `docs/INCIDENT_RUNBOOK.md` §5's own recovery checklist holds this
   deployment to).
3. Create a real tenant against the deployed instance's admin API and drive
   one order through it exactly as a self-hosted install's own smoke test
   would - if key material genuinely round-trips through the
   now-out-of-process `SocketKeyCustody` (unseal, scan-address derivation,
   output matching), this is where a wiring mistake would surface, not in
   any unit test.
4. `key-custody-server`'s own `tests/socket_key_custody.rs` (17 tests) and
   the engine's own `http`/`scanner` integration tests that run against
   `TestEngineConfig::with_socket_key_custody()` were the actual regression
   suite 2.1.3 shipped - re-running `cargo test --workspace` on a dev box
   proves the *code path* is still correct, but per this step's own test
   text ("against the deployed instance over the network") that's not a
   substitute for step 3 above: those tests exercise the two processes
   talking to each other on a dev machine, not this specific deployed
   instance, its real config, and its real systemd process boundary.

## What this deployment does *not* change

Per `docs/INCIDENT_RUNBOOK.md`: SEV-SNP protects memory confidentiality
against a host-level/hypervisor attacker who does *not* have a foothold
inside the guest VM itself. It is not a substitute for the guest's own
process hardening (the systemd sandboxing directives in both `.service`
files above), and a compromise that gains code execution *inside* the guest
still exposes everything `PlainKeyCustody` (running inside
`key-custody-server`, unchanged by any of this) holds in cleartext memory,
exactly as `docs/DESIGN.md` §6.1 describes. What SEV-SNP adds is real and
worth having - it closes the "rogue admin or compromised hypervisor on the
provider's side" case `docs/DESIGN.md` §6.1 names explicitly - just don't
describe an incident as "contained because it's on SEV-SNP hardware" if the
foothold was inside the guest.
