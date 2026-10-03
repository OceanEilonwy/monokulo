# Decisions: SEV-SNP key custody work pack

The plan: drop the socket key custody backend (the plain backend's code in a
separate process, with keys stored in the clear), keep the router and every
other abstraction, add a real SEV-SNP backend, and send merchants' keys to it
encrypted (browser via WebAssembly, `key-custody-cli` without JavaScript), with
the CLI released alongside each monokulo version. These are the calls made
along the way, newest last.

Format: **Decision**, **Alternatives**, **Why**.

### 1. Sealing is bound to the launch measurement; upgrades hand the key over
- **Decision:** The sealing key is derived (`SNP_GET_DERIVED_KEY`) from the chip, the
  guest policy and the **measurement**. It wraps a random master key, stored once per
  image in `snp_master_keys`; stores' keys are sealed under the master key. A new image
  gets the master key from the engine it replaces by an attested handoff
  (`POST /api/v1/admin/key-custody/handoff`, `key_custody.snp_handoff_url`), which goes
  only to an image signed by the trusted ID key at the same security version or later.
- **Alternatives:** Option (b) as planned: derive from family ID, image ID and SVN.
- **Why:** The firmware's derived-key fields are policy, image ID, family ID,
  measurement, SVN, TCB and launch-mitigation vector; the **ID key is not one of them**.
  A host could launch its own image with an ID block it signed itself, carrying the same
  family/image IDs and SVN, and derive the same key. The measurement it can't fake. The
  ID key is still what trust rests on: reports carry its digest, the engine checks its
  own at start, clients check it, and the handoff checks it. So (b)'s outcome (signed
  upgrades keep every store's keys) holds, securely.

### 2. The snp backend runs in the engine's process
- **Decision:** `SnpKeyCustody` lives in the `key-custody` crate and runs inside the
  engine, in the confidential VM. Its in-use registry is the plain backend's.
- **Alternatives:** A separate process, as the socket backend was.
- **Why:** SEV-SNP protects the whole VM; a second process inside it adds nothing
  against the host and brings back the socket's complexity.

### 3. Every abstraction is kept, including ones only a remote backend needs
- **Decision:** `check_state` epochs, `unseal_and_register_idempotent`, router
  `replace`/`free_handles` stay, with tests renamed from `socket` to `snp`.
- **Alternatives:** Remove them as dead now that both backends are in-process.
- **Why:** You asked to keep the router and all the abstractions. They stay ready for
  a backend in another process (an AWS Nitro enclave would be one). Only code that
  existed purely to cross the socket was removed: `WalletHandle::as_bytes/from_bytes`
  and `ScanInput::to_bytes/from_bytes`, with their tests.

### 4. The key custody crate holds the boundary, the backends and the transport
- **Decision:** `crates/key-custody`: the trait and types (from `shared`), plain, router,
  output matching (from the engine), snp and transport. Feature `backends` (default) is
  the engine's; without it only `transport` builds, for `key-custody-cli` and, with
  `wasm`, the browser. The engine re-exports it as `engine::key_custody`.
- **Alternatives:** Separate crates per piece.
- **Why:** One concept, one crate, as you asked; the feature split keeps monero, tokio
  and the backends out of the WebAssembly module.

### 5. The AMD chain is verified in pure Rust, offline, with the time passed in
- **Decision:** `snp-attest` checks RSASSA-PSS-SHA384 with the `rsa` crate instead of
  `x509-parser`'s ring-backed `verify`; `verify_evidence` takes the certificates and the
  current time. KDS fetching moved behind a `kds` feature.
- **Alternatives:** Keep ring; write a separate JavaScript verifier.
- **Why:** The same verifier must run in the browser. ring's C doesn't build for
  `wasm32-unknown-unknown` without extra toolchains, and `SystemTime::now` panics there.
  AMD's real ASK and revocation lists still verify (existing fixture tests).

### 6. WebAssembly without wasm-bindgen, built by monokulo's build script
- **Decision:** `key-custody`'s `wasm` feature exports `alloc`, `dealloc`, `seal`,
  `output_ptr`, `output_len` (JSON in, JSON out) and imports only `env.fill_random`
  (`crypto.getRandomValues`), which `getrandom`'s custom backend uses
  (`.cargo/config.toml`). Monokulo's `build.rs` builds it with a nested
  `cargo build --offline --locked --target wasm32-unknown-unknown`, size-optimised,
  clearing the outer build's `RUSTFLAGS` (coverage) and wrappers; `key-custody` is a
  build-dependency so its crates are fetched first. `rust-toolchain.toml` and every CI
  toolchain step add the wasm32 target.
- **Alternatives:** wasm-bindgen/wasm-pack; committing the built module.
- **Why:** No new toolchain beyond the target, the same pattern as the POS app
  (built into `OUT_DIR`, never committed), and a nightly-built artifact in git would
  change daily. The module is about 520 KB.

### 7. Challenges are single-use, an hour long, kept in memory
- **Decision:** The backend issues a 32-byte challenge per bundle, for one action and
  (moving) one store, and accepts it once within an hour. They are lost when the engine
  restarts, as is its receiving key, so forms open before a restart must be reloaded.
- **Alternatives:** Persist challenges; bind creation to a store.
- **Why:** A restart changes the backend's key anyway. A new store has no identity yet to
  bind to; moving is also protected by the same-wallet check.

### 8. snp refuses keys in the clear at three layers
- **Decision:** `SnpKeyCustody::register_wallet` refuses; the engine's create/move
  refuse `view_key_hex` for a backend that takes only encrypted keys; monokulo refuses
  typed keys for `snp` without forwarding them, saying they weren't sent.
- **Why:** You decided raw keys are refused; doing it in monokulo too means a keyboard
  mistake never puts keys on the wire.

### 9. Settings
- **Decision:** `key_custody.snp_product`, `snp_device`, `snp_trusted_id_key`,
  `snp_min_guest_svn`, `snp_handoff_url` apply at a restart and aren't editable through
  the API or admin page; monokulo's `key_custody.cli_download_url` and
  `key_custody.cli_source_url` (templates with `{version}`, `{file}`, `{ref}`) likewise.
  The Custody admin tab gains a "key-custody-cli downloads" group for the latter.
- **Why:** Trust anchors and the links merchants download from belong to the
  deployment, not to whoever holds an admin session. Invalid values stop the process,
  as for every setting.

### 10. An snp backend that can't start is a stand-in, not a failed section
- **Decision:** If the backend can't start (no product, no trusted ID key, not an
  SEV-SNP guest, an untrusted image), the router gets `Unstarted`, which answers every
  call "unavailable: <why>", and saving the setting returns a warning.
- **Alternatives:** Fail the custody section.
- **Why:** With `StartDegraded`, a failed section isn't installed at all, which would
  take plain stores down too.

### 11. The engine fetches AMD's certificates for its own report
- **Decision:** An upkeep loop fetches the ASK, VCEK and CRL from AMD's KDS once the
  backend starts, retries every 30 s until it has them, and refreshes them every 12
  hours. Bundles carry them.
- **Why:** Clients then need no network access to AMD. The engine needs outbound HTTPS
  to `kdsintf.amd.com` (documented in the deploy guide).

### 12. Monokulo serves bundles for the CLI
- **Decision:** Each bundle monokulo hands out with a form is kept for an hour and
  served at `GET /key-custody/bundles/{id}` (random 128-bit id, no session needed),
  linked from the form; the command shown uses the absolute URL when `public_url` is
  set, and a saved file otherwise.
- **Why:** A merchant on another computer can fetch it with the CLI; a bundle holds
  nothing secret.

### 13. Which ID key the browser trusts comes from the engine; the CLI's is built in
- **Decision:** The engine's bundle answer includes its trusted digest and whether it is
  the official one; monokulo passes it to the page. The CLI uses the digest built into
  it unless `--trust-id-key` is given, and never reads one from the bundle.
- **Why:** The browser already trusts monokulo for the code it runs, so this adds no
  trust. The CLI's guarantee is that it doesn't depend on the site.

### 14. The official ID key digest is empty until you make the key
- **Decision:** `crates/key-custody/src/official_id_key_digest.txt` ships empty.
  `cargo xtask snp-id-key` makes the key, prints it for the `SNP_ID_KEY` secret and
  writes the digest. Until then the engine's `snp` backend needs
  `key_custody.snp_trusted_id_key`, and the CLI needs `--trust-id-key`.
- **Why:** The key should be made by you and stored in your repository's secrets, not by
  an agent.

### 15. ID blocks are signed by a manual workflow; image building is out of scope
- **Decision:** `snp-attest::id_block` lays out and signs ID blocks (SNP ABI
  `ID_BLOCK`/`ID_AUTH_INFO`, ECDSA P-384, no author key); `cargo xtask snp-id-block`
  and the `snp-id-block` workflow (manual dispatch, `SNP_ID_KEY`) produce
  `id-block`/`id-auth` files, raw and base64, for QEMU.
- **Alternatives:** Sign in the release workflow.
- **Why:** The measurement belongs to a VM image, which this repository doesn't build
  (it depends on distribution and hypervisor). The deploy guide says the image must
  measure the engine (dm-verity root, or the engine in the initrd), or attestation
  names nothing.

### 16. One workspace version; the CLI is released with every tag
- **Decision:** `[workspace.package] version` and `repository`, inherited by every crate.
  On a `v*` tag CI checks the tag names that version. A new `publish-cli` job builds
  `key-custody-cli` for Linux x86-64/ARM64, macOS ARM/Intel and Windows x86-64, packaged
  as `key-custody-cli-{version}-{target}.tar.gz` (`.zip` on Windows) with SHA-256 files
  and, on tags, build provenance; the release job adds them. Builds get
  `MONOKULO_RELEASE_TAG` and `MONOKULO_GIT_COMMIT`, from which monokulo builds its
  links: a release links its own CLI files, any other build links the source at its
  commit.
- **Why:** An older monokulo must link the CLI built with it.

### 17. The browser e2e spec covers plain, and snp where it can't run
- **Decision:** `real-2-store-key-storage.spec.js` now creates a plain store and
  checks that turning `snp` on without SEV-SNP hardware is reported, that the forms say
  encrypted key entry isn't available, and that the plain store carries on.
- **Why:** As you decided; snp itself is covered by Rust tests against a stand-in
  security processor, end to end through monokulo's no-JS path.

### 18. What hasn't been run on real hardware
- `/dev/sev-guest` ioctl layouts, derived keys and ID block format follow Linux's
  `sev-guest.h` and AMD's ABI spec. A test exercises the real device and skips
  elsewhere. The engine's start-up check prints the report's ID key digest against the
  configured one, so a format mismatch shows at the first real launch.
- The browser path can't be run end to end without an AMD-signed report. The WebAssembly
  glue was smoke-tested in Node; what it calls is the Rust tested natively.

### 19. Known limits left as they are
- **Rollback with an old database copy:** an old trusted image, with a database copy
  holding its own wrap, can still unwrap the master key. The handoff never goes down in
  version, but the master key isn't rotated when the version rises.
- **Handoff endpoint** is protected by the engine token like every route, and
  cryptographically by the attestation check; its answer is encrypted to the asking
  engine alone.
- The engine test `the_engine_starts_without_a_file_and_follows_one_and_its_options`
  failed once under load (it races on `free_port`); it passes alone and repeatedly. This
  predates this work.

## After the review

A review of the pull request (a separate agent, reading the change cold) found the
following; each was fixed as described unless it says otherwise.

### 20. A handoff answer must be attested by the engine handing over
- **Finding:** the successor took any envelope sealed to its handoff key. HPKE's base
  mode doesn't say who sealed it, and the handoff request travels in the clear to
  whatever `snp_handoff_url` names. A host could empty the wraps, point the setting at
  its own server and plant a master key it knows; merchants re-entering their keys
  would then seal them under it.
- **Decision:** the answer (`HandoffAnswer`) carries a report of the answering engine
  whose REPORT_DATA binds the successor's key and the envelope (challenge, encapsulated
  key, ciphertext). The successor checks it against AMD's chain and takes it only from
  an engine signed by its own ID key, at the minimum security version and firmware.

### 21. The launch's own ID key is mixed into the wrapping key, and handoffs follow it
- **Finding:** the trusted ID key was a setting the host controls, and the wrapping key
  didn't depend on it. The host could relaunch the genuine image under an ID block of
  its own, configured to trust its key: same measurement, same derived key, the real
  master key unwrapped. That engine would then hand it to any image the host signed.
- **Decision:** the wrapping key is HMAC(derived key, label ‖ the ID key digest in this
  launch's own report). The same image under another ID key gets another wrapping key
  and waits for a handoff. Handoffs, both ways, go only to images signed by the engine's
  own attested ID key, not the configured one (which must match it to start at all).

### 22. A firmware floor (`key_custody.snp_min_tcb`)
- **Finding:** nothing refused reports from old firmware with known SEV-SNP breaks, and
  AMD keeps certifying old TCBs.
- **Decision:** `TrustPolicy` gains `min_tcb`: the bootloader, TEE, SNP and microcode
  patch levels the reported TCB must reach. The engine setting is checked at start and
  in handoffs, sent to the browser with the bundle, and put into the printed CLI command
  (`--min-tcb`). There is no built-in default: the right levels depend on the product
  and AMD's bulletins, and a wrong guess would either refuse every engine or protect
  nothing. The deploy guide says to set it.

### 23. A revocation list must be in force
- **Finding:** an old, validly signed revocation list could be replayed past a revocation.
- **Decision:** a list whose next update has passed, or that isn't issued yet, is
  refused. The engine fetches a new one twice a day.

### 24. Challenges are used up only by an envelope that opens
- **Finding:** anyone who saw a bundle could spend its challenge with a garbage envelope.
- **Decision:** a challenge is removed once its envelope opens (atomically, so of two
  copies only one registers). Issuing is not rate-limited per user beyond monokulo's
  existing limits; each bundle needs a signed-in form load.

### 25. Typed keys aren't sent when the default backend is unknown
- **Finding:** with no backend named and monokulo's status cache empty, typed keys went
  to the engine, which might default to `snp`.
- **Decision:** monokulo reads the engine's status first; if it still can't tell, it
  refuses without forwarding.

### 26. Release images carry the release identity
- **Finding:** Docker images had no release tag or commit, so their forms linked no CLI
  download.
- **Decision:** the Dockerfile takes `MONOKULO_RELEASE_TAG` and `MONOKULO_GIT_COMMIT` as
  build arguments, and CI passes them.

### 27. Kept: the browser and the printed CLI command use the engine's trusted digest
- **Finding:** the page and the printed `--trust-id-key` take the digest from the
  engine's configuration, which the host controls.
- **Decision:** kept as you decided (one source of truth, the digest in the command,
  with a warning to confirm it with the operator). Decision 21 removes what made it
  dangerous for the official key: a host-configured key no longer unwraps or receives the
  real master key. For an instance on its own key, merchants who want the strongest
  guarantee get the digest from the operator, not the page; the page says so.

Also fixed: the CLI's packaging step no longer needs Python on Windows; the report fields
only tests read (family/image ID, author key digest) and the CLI's test-only
`check_against` were removed; a stale comment named the socket backend.

### 28. SEV-SNP runs the engine standalone
- **Decision:** `main` now runs the engine inside monokulo by default. The snp backend works in either mode, but an engine inside monokulo serves no HTTP API of its own, so it can't answer an upgraded engine's handoff (`POST /api/v1/admin/key-custody/handoff`). The SEV-SNP deploy guide runs the engine standalone (`monokulo-engine`), so upgrades keep the master key.
- **Why:** answering a handoff through monokulo's public address would expose an engine route publicly, which the engine boundary rules out.

## After the security pass

A second review looked only at whether merchants' keys and payments are safe. These
changes follow from it without changing anything decided earlier; the open items at the
end need a decision.

### 29. Monokulo decides "official" itself, and refuses a bad trust answer
- **Finding:** the page's "official ID key" flag and the digest came from the engine's
  answer, so a host could have a custom key presented as official.
- **Decision:** monokulo compiles in the official digest and compares; the engine's
  answer only supplies the configured digest, minimum security version and TCB floor,
  each parsed, and a form isn't shown if any doesn't parse.

### 30. The printed CLI command quotes every value
- **Finding:** values from the engine were pasted into a shell command unquoted.
- **Decision:** each is single-quoted for the shell (`shell_word`).

### 31. The private view key is never put back in a page
- **Finding:** a form that failed validation re-filled the view key.
- **Decision:** everything else is re-filled; the view key is typed again.

### 32. The guest device is fixed, and its answers are checked
- **Finding:** `snp_device` was a host-controlled setting naming the device that
  derives the sealing key; a status left unwritten read as success.
- **Decision:** the setting is removed and `/dev/sev-guest` is fixed. The status is
  pre-filled so the firmware must overwrite it, and an all-zero derived key is refused.
  `root_key_select` stays 0 (the VCEK root, as older firmware expects).

### 33. Guests a migration agent could export are refused
- **Decision:** policy bit 18 (MIGRATE_MA) is refused by the bundle check, at the
  engine's start, and by `cargo xtask snp-id-block`, which also refuses DEBUG.

### 34. No going back to an older image
- **Finding:** the security version stored with each wrap was never used, so a host
  could run an older, flawed image after an upgrade.
- **Decision:** an image older than any wrap in the database waits instead of taking
  the master key. A restored copy of the database from before the upgrade still defeats
  this; documented.

### 35. The ID block workflow is locked down
- **Decision:** `SNP_ID_KEY` belongs to the `snp-id-key` environment (required
  reviewers, main and tags only), the job runs only from main or a version tag, restores
  no cache and uses actions pinned to commits. **To do on GitHub:** create the
  environment and move the secret into it.

### 36. Documented limits
- The host can learn which payments are a store's by feeding the engine crafted blocks.
- The engine's clock is the host's; merchants' clients check against their own.
- The TCB floor is a setting; the CLI enforces the merchant's own.
- The forms show a `gh attestation verify` command for the CLI download.

### Open, needing a decision
- Binding the TCB version into the derived key, which needs testing on hardware.
- Authenticating what the engine scans, against the payment-linking limit above.
- TLS (or the same confidential VM) between monokulo and the engine.
- A per-user limit on issued bundles.

## Your decisions on the security pass

### 37. The trust policy is monokulo's own (you chose option A)
- **Decision:** monokulo has four settings of its own in the registry, shown in the snp
  backend's section of the Custody tab: `key_custody.snp_entry_id_key`,
  `snp_entry_min_guest_svn`, `snp_entry_min_tcb` and `snp_entry_required`. The key entry
  forms and the printed CLI command use them; the engine's bundle answer no longer
  carries a trust policy, and the engine reports its own on `/status`
  (`key_custody_snp_trust`). This reverses decision 27.
- **When they disagree** (your addition): `snp` can't be chosen anywhere, keys for it
  are refused, a store whose engine default is `snp` goes to the next usable backend
  (named in the request, so the engine can't pick `snp`), and the status page shows a
  red alert. Operators see what differs there and in the alert bar; others see only
  that SEV-SNP key storage is unavailable.
- **Saving** (your addition): the settings are editable on the admin page and apply at
  once. A save is checked against the engine's `/status` first and refused, each
  differing setting named beside its field, if they differ. A changed value isn't saved
  while the engine doesn't answer; an unchanged one doesn't need it.
- **`snp_entry_required`:** typed keys are never sent on and no plain choice is offered,
  whatever the engine reports, so a host can't get keys typed in the clear by
  reporting `snp` absent. It can't be turned on unless the engine has `snp` enabled
  (the save check), and a build without an official ID key needs `snp_entry_id_key`
  set for it.
- **Also:** the Custody tab drew the backends' sections once per group; they are drawn
  once now, with monokulo's own settings inside the snp one.

