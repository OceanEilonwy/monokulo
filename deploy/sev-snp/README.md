# Deploying the engine inside an AMD SEV-SNP confidential VM

With the engine in an SEV-SNP confidential VM and the `snp` key custody
backend enabled (`docs/DESIGN.md` §6.3-6.5), stores' private view keys are:

- encrypted in memory by the hardware, against the host and the hypervisor;
- sealed at rest under a master key only this engine image, on this chip, can
  unwrap (a stolen `engine.db` gives nothing);
- sent by merchants encrypted to the engine itself, after their browser or
  `key-custody-cli` has checked the engine's attestation: monokulo only relays
  them.

What makes this mean something is the **engine image**: the measurement the
firmware reports must cover the engine binary, and the image's ID block must be
signed by the ID key merchants trust. The steps below are in that order.

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
   deploying onto it defeats the point of this deployment.

## 2. Build a measured engine image

The guest's launch measurement covers what the firmware loads: OVMF and, with
direct boot, the kernel, initrd and kernel command line. For it to name *this
engine*, the engine must be part of what is measured: build the root
filesystem (with `/opt/monokulo/bin/monokulo-engine` and its unit) as a
read-only dm-verity image and put its root hash on the measured kernel command
line, or put the engine in the measured initrd. An engine installed on an
unmeasured disk is not attested, whatever the report says.

The `snp` backend is built only into an engine compiled with the `snp`
feature; release binaries and the Docker image leave it out, and refuse
`snp` in `key_custody.enabled_backends`. Build the image's engine with it:

```sh
cargo build --release --locked -p engine --bin monokulo-engine --features snp
# (ZMQ notifications come with it: zmq is a default feature)
```

Building the image is outside this repository (it depends on your
distribution and hypervisor). Compute its launch measurement with the image's
exact launch parameters (vCPU count and type, OVMF, kernel, initrd, command
line, guest policy), for example with
[`sev-snp-measure`](https://github.com/virtee/sev-snp-measure):

```sh
sev-snp-measure --mode snp --vcpus 4 --vcpu-type EPYC-v4 \
    --ovmf OVMF.fd --kernel vmlinuz --initrd initrd.img --append "$CMDLINE"
```

## 3. Sign the image's ID block

The ID block names the image (its measurement, family and image IDs and
security version, `guest_svn`) and is signed with the engine **ID key**. Every
attestation report then carries the ID key's digest, which is what the engine,
merchants' browsers and `key-custody-cli` trust.

- The official ID key: its digest is built into this repository
  (`crates/key-custody/src/official_id_key_digest.txt`, made once with
  `cargo xtask snp-id-key`), and its private key is the repository secret
  `SNP_ID_KEY`. Sign with the **snp-id-block** workflow (Actions, run it with
  the measurement and security version); it uploads the signed files.
- Your own ID key (a fork, or your own builds): `cargo xtask snp-id-key`
  prints a key and writes its digest; keep the key secret, then sign with
  `SNP_ID_KEY=<key> cargo xtask snp-id-block --measurement <hex> --guest-svn <n>
  --out id/`, and set `key_custody.snp_trusted_id_key` (below) to the digest.
  Merchants pass the same digest to `key-custody-cli` with `--trust-id-key`;
  give it to them yourself, not through the site.

**Raise `--guest-svn`** with every release that fixes a security problem in the
image: the engine never hands its master key to a lower version, and
`key_custody.snp_min_guest_svn` refuses lower versions outright.

Launch the guest with the signed block, e.g. with QEMU:

```sh
-object sev-snp-guest,id=sev0,policy=0x30000,cbitpos=51,reduced-phys-bits=1,\
id-block=$(cat id/id-block.b64),id-auth=$(cat id/id-auth.b64)
```

The `policy` must be the one given to `snp-id-block` (`--policy`, default
`30000`), and must not allow debugging.

## 4. Configure the engine

`/etc/monokulo/engine.toml` (`monokulo-engine --init --options x.toml` writes
one describing every setting):

```toml
[key_custody]
enabled_backends = ["plain", "snp"]
default_backend = "snp"
snp_product = "Genoa"            # Milan, Genoa or Turin: the host's EPYC generation
# snp_trusted_id_key = "..."     # only for your own ID key; empty trusts the official one
# snp_min_guest_svn = 1
snp_min_tcb = "10,0,23,213"      # bootloader,tee,snp,microcode: the levels AMD's bulletins name for your product
# snp_handoff_url = "http://10.0.0.5:8443"   # only when upgrading, see §6
```

`/etc/monokulo/engine.env` holds `ENGINE_TOKEN` (the same value monokulo has as
`MONOKULO_ENGINE_TOKEN`). The `snp_*` settings apply at a restart and can't be
changed from monokulo's admin page. Set `snp_min_tcb` to the firmware levels that
fix the SEV-SNP issues AMD has published for your EPYC generation (§1 gives the
levels your host reports): the engine, merchants' key entry and handoffs then
refuse anything older. The example above is a placeholder, not a recommendation.

Each release also carries its own floor per product
(`crates/key-custody/src/release_tcb_floors.txt`), which no setting lowers: an
engine on older firmware doesn't start, and no client sends keys to one. The
master key is wrapped at the platform's committed firmware version, so a
firmware rollback leaves the backend waiting (the status page says why) until
the firmware is updated again. After a firmware update, restart the engine: it
wraps the key again at the new version, and older firmware can no longer open
it. **Commit firmware updates** (`snphost commit`, or your provider's
equivalent) so the old firmware can't be loaded again.

**Check on first deployment** that the engine starts with its master key ready
(the status page shows the `snp` backend without a "waiting" reason) and that a
restart keeps it ready: binding the firmware version into the derived key is
tested against a stand-in security processor, not yet against real hardware.

Then set the same policy in monokulo, whose key entry forms check every
engine against it (`monokulo.toml`, or the snp section of the admin page's
Custody tab):

```toml
[key_custody]
# snp_entry_id_key = "..."       # the engine's snp_trusted_id_key; empty for the official one
# snp_entry_min_guest_svn = 1    # the engine's snp_min_guest_svn
snp_entry_min_tcb = "10,0,23,213" # the engine's snp_min_tcb
snp_entry_required = true         # never take a store's keys in the clear
```

The admin page refuses to save values that differ from the engine's. If they
come to differ anyway (one side's options file edited, the engine restarted
with new settings), SEV-SNP key storage stops being offered and the status page
shows a red alert naming what differs. With `snp_entry_required`, no form takes
keys in the clear, whatever the engine says.

**Keep the engine private.** Its `server.bind` defaults to `127.0.0.1:8443`;
bind it only to a private address monokulo can reach, never a public one.

**Keep the link from monokulo private too.** The engine serves plain HTTP,
and every request from monokulo carries the engine token; `plain` stores'
keys and every answer monokulo acts on (payments, `/status`) cross it. Unless
monokulo runs on the same machine, carry it over a private network or a
tunnel that ends inside the guest: WireGuard, or a TLS proxy (stunnel, Caddy)
in the guest in front of `server.bind`, with monokulo's `engine.url` set to
its `https://` address. This guards against the network, not the host: the
host sees the link and holds the token anyway. Keys for `snp` stores are
encrypted to the attested engine and don't depend on it.
The engine needs outbound HTTPS to `kdsintf.amd.com` (AMD's certificates for
its own report, which merchants' clients check) and to its Monero nodes.

## 5. Install and start

```sh
useradd --system --no-create-home --shell /usr/sbin/nologin monokulo
echo 'KERNEL=="sev-guest", GROUP="monokulo", MODE="0660"' > /etc/udev/rules.d/90-sev-guest.rules
udevadm trigger
cp deploy/sev-snp/monokulo-engine.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now monokulo-engine.service
```

(When the engine is part of the measured image, as §2 requires, these belong in
the image build rather than on a running guest.)

At its first start on an empty database the `snp` backend makes the master
key. Check:

1. `journalctl -u monokulo-engine`: "fetched AMD's certificates for this
   engine's report", and no "snp backend can't start".
2. Monokulo's status page: the `snp` backend answers.
3. A store connected with SEV-SNP key storage, from a browser and once with
   `key-custody-cli` (the form links it), takes an order and sees its payment.

The engine checks its own report before the backend starts: an image
launched without the ID block, signed by another key, below
`snp_min_guest_svn`, or debuggable leaves the backend off and says which.

## 6. Upgrading the engine image

Run the engine standalone (`monokulo-engine`, as above), not inside
monokulo: only a standalone engine serves the handoff route below.

A new image has a new measurement, so it can't unwrap the master key itself:
the engine it replaces hands it over.

1. Build, measure and sign the new image (§2, §3), with the same or a higher
   `--guest-svn`.
2. Start it with a copy of the database and `snp_handoff_url` set to the
   running engine's private address (same engine token). It waits for the
   master key, asks for it, and takes it within seconds; the old engine
   checks against AMD's chain that the new one is a trusted image at its own
   security version or later before handing over.
3. Switch monokulo to the new engine, stop the old one, and remove
   `snp_handoff_url`.

Moving to another machine works the same way: wraps are bound to the chip. If
no engine can hand over any more, see `docs/INCIDENT_RUNBOOK.md` §7.

## What this does *not* protect against

- **Code execution inside the guest** reads what the engine reads: SEV-SNP
  protects against the host, not against a compromised engine.
- **Keys typed into a monokulo page served by a compromised monokulo**: the
  browser runs the code monokulo serves. `key-custody-cli` checks the engine
  itself and doesn't have this gap; the key entry forms say so.
- **Whoever holds the ID key** can sign an image that is trusted with keys.
  Keep `SNP_ID_KEY` to the release process: a secret of the `snp-id-key`
  environment, with required reviewers, available to `main` and version tags
  only (`.github/workflows/snp-id-block.yml`).
- **A store's payment history.** The keys stay sealed, but the engine scans
  with them for whoever asks, and the host can ask: by feeding crafted blocks
  from `monerod`, by editing the database on its disk (orders at chosen
  subaddress indices, rewound scan cursors) and reading the payments and
  amounts the engine records there, and through the admin API with the engine
  token. Assume the host can learn which payments are a store's and how much
  they were for (`docs/DESIGN.md` §6.5, Limits).
- **The engine's clock** is the host's: it can make a stale revocation list
  look current to the engine. Merchants' clients check bundles against their
  own clocks.
- **A restored copy of the database** from before an upgrade lets the host run
  the older image again. Without such a copy an older image is refused once a
  newer one has used the database.
- **A tampered `key-custody-cli` download**: check it with
  `gh attestation verify <file> --repo <owner>/<repo>`, as the key entry forms
  say, not only against its checksum.
