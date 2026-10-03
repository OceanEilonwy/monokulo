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
