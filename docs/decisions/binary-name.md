# Decisions: the executable's name, monokulo-pay

Made on 2026-11-13 without the owner to ask, at the owner's request to
rename the server's executable after the repository was renamed
`monokulo-pay` (from `monokulo`). Each says what was chosen, why, and what
else was possible, so any of them can be reversed knowingly. The brief
(only the executable changes; not the package, the crate, the OpenWrt
package, the config paths, the `MONOKULO_*` variables or the product's
name) is the owner's, not re-decided here.

1. **A `[[bin]]` named `monokulo-pay` over `src/main.rs` in
   `crates/monokulo/Cargo.toml`; the package stays `monokulo`.** So
   `cargo … -p monokulo`, `monokulo::…` paths, the `monokulo/snp` feature
   and nextest's `binary_id(monokulo)` (the library) all stay put, and only
   `--bin` and the built path change. The crate had no `[[bin]]` before
   (Cargo's automatic one, named for the package); declaring one turns off
   no other target (the examples and tests are still discovered).
   Rejected: renaming the package, which would move every crate path and
   every `-p monokulo` in CI and the docs for a name users never type.
2. **clap's command is named `monokulo-pay` too**, and the help's examples
   say `monokulo-pay`, so `--help` and `--version` name the executable a
   user runs. The options file keeps its name and folder
   (`~/.config/monokulo/monokulo.toml`), as the brief says.
3. **The release zips are `monokulo-pay-{version}-{target}.zip`, holding
   `monokulo-pay` (`monokulo-pay.exe`)**, named for the executable as the
   services' zips are (`monokulo-push-relay-…`, `monokulo-bell-…`). The
   GitHub release jobs download `monokulo-pay-[0-9]*` and
   `monokulo-pay-main-*`, which can't match the OpenWrt artifacts
   (`monokulo-openwrt-*`) or the services'. The latest-main job removes
   assets a build no longer makes, so its old `monokulo-main-*.zip` links
   stop working at the first push to main after this merges; the install
   page and the release notes link the release pages, not the files, and
   nothing in the repository links a file by name. Rejected: keeping
   `monokulo-{version}-{target}.zip` with `monokulo-pay` inside, which
   keeps old file links but names the zip for something it doesn't hold.
4. **`monokulo` stays as a link to `monokulo-pay` wherever the repository
   installs it**: `/usr/local/bin/monokulo` in the Docker image and
   `/usr/bin/monokulo` in the OpenWrt package (`$(LN)`, so `apk del` takes
   it with the package; `test-openwrt.sh` checks both the link and its
   removal). The image's `CMD`, `compose.yaml` and the init script run
   `monokulo-pay`, so nothing of ours depends on the link and it can be
   dropped without touching them. A desktop zip carries no link (a zip
   can't hold one Windows understands); its users run what they unzipped.
5. **The docs say `monokulo-pay` in every command, and say once that
   `monokulo` remains an alias for now** (the README's Get started, the
   install page's Download and extract). When the alias goes is the
   owner's call, not made here; dropping it is the two link lines, the
   test's check and those two notes.
6. **Prose that names monokulo the service stays "monokulo"**: comments,
   the docs' "inside monokulo", the release job's id, log service names,
   the Tor nickname and the user agent. Only places where a person or a
   script types or finds the executable changed.
7. **Links to `github.com/OceanEilonwy/monokulo` now say
   `…/monokulo-pay`**, including the workspace's `repository`, from which
   the key-custody-cli download and source defaults are made
   (`CARGO_PKG_REPOSITORY`), the LuCI pages' repository links and the
   site header. GitHub redirects the old address, so links already out
   keep working. The docs site's GitHub Pages address (`…github.io/…`) and
   the image name `ghcr.io/<owner>/monokulo` are not the executable and
   stay as they are.
