# monokulo-push-relay

The push relay of [Monokulo](https://github.com/OceanEilonwy/monokulo), on
its own: it takes the Web Push that monokulo and the Monokular Bell send to
a till app and hands it on to Apple's (APNs) or Google's (FCM) push service,
under the app's own keys. An app registers its device token here and gets
a Web Push endpoint back; the relay keeps only those handles, never a push.
monokulo can run the same relay inside itself (`relay.enabled`); this is
for running it somewhere else. Everything about it, the API included:
<https://docs.monokulo.com/push-relay/>.

## What is in the zip

| File | What it is |
| --- | --- |
| `monokulo-push-relay` (`.exe` on Windows) | the relay |
| `relay.toml` | a starter options file: every setting with what it is for and its default, commented out |
| `service/monokulo-push-relay.service` | a systemd unit (Linux) |
| `service/com.monokulo.push-relay.plist` | a launchd daemon (macOS) |
| `service/install-windows-task.ps1` | installs it as a Windows startup task |

The zip's `.sha256` beside it on the release page is its SHA-256.

## Run it

Set the relay's https address and the apps it serves in `relay.toml`
(`--init` writes a fresh one to `~/.config/monokulo/relay.toml`):

```toml
[server]
public_uri = "https://relay.example.org"

[handles.apps.monokular]
apns_topic = "com.example.monokular"
fcm_project_id = "monokular-12345"

[apns]
team_id = "ABCDE12345"
key_id = "FGHIJ67890"
```

The secrets come from the environment only, never the options file or the
command line:

| Variable | |
| --- | --- |
| `MONOKULO_RELAY_HANDLE_KEY` | required: 64 hex characters (`openssl rand -hex 32`); every endpoint is made under it, so keep it, apart from the database |
| `MONOKULO_RELAY_APNS_KEY` | the APNs key, the `AuthKey_….p8` file whole, for iPhones |
| `MONOKULO_RELAY_FCM_CREDENTIALS` | the Firebase service account's JSON whole, for Android |

```sh
(umask 077; openssl rand -hex 32 > handle.key)   # once; keep it
export MONOKULO_RELAY_HANDLE_KEY="$(cat handle.key)"
export MONOKULO_RELAY_APNS_KEY="$(cat AuthKey_FGHIJ67890.p8)"
./monokulo-push-relay --options relay.toml --check   # what it would run on, then OK; or why not
./monokulo-push-relay --options relay.toml
```

It listens on `0.0.0.0:8444` (`server.listen`) and keeps its handles in
`~/.local/share/monokulo/push-relay.db` (`store.path`). Any setting can be
given on the command line too (`--help` lists them); anything invalid stops
it before it listens, with the reason.

**TLS:** the relay serves plain HTTP. Put an https reverse proxy in front
(Caddy, nginx), serving `server.public_uri`, and name it in
`server.trusted_proxies` when it is on another machine. The relay itself
uses TLS only to reach APNs and FCM, trusting the machine's certificate
store.

**Status:** `curl -s http://127.0.0.1:8446/status` (`status.listen`, this
machine only) says what it is doing as JSON: its address, its apps, the
handles it holds, whether APNs and FCM are ready, and the last push it
handed on.

The binaries aren't signed. On macOS clear the quarantine of a downloaded
one once (`xattr -d com.apple.quarantine monokulo-push-relay`); on Windows
SmartScreen may ask first (More info > Run anyway).

## Run it as a service

Each file in `service/` says how to install it in its first lines. All
three run `--options` and the store path at a fixed place, so the relay
needs no home directory.

**Linux (systemd).** `service/monokulo-push-relay.service` runs it as a
user systemd makes for it, its handles in `/var/lib/monokulo/push-relay/`,
its options in `/etc/monokulo/relay.toml` and its secrets in the
root-only `/etc/monokulo/monokulo-push-relay.env`, checking the settings
(`--check`) before each start. `journalctl -u monokulo-push-relay` is its log.

**macOS (launchd).** `service/com.monokulo.push-relay.plist` runs it at
boot as `nobody`:

```sh
dir="/Library/Application Support/monokulo"
sudo mkdir -p /usr/local/bin "$dir/push-relay"
sudo install -m 755 monokulo-push-relay /usr/local/bin/
sudo xattr -d com.apple.quarantine /usr/local/bin/monokulo-push-relay
sudo install -m 644 relay.toml "$dir/relay.toml"   # then set server.public_uri and your apps
sudo chown nobody "$dir/push-relay" && sudo chmod 700 "$dir/push-relay"
sudo install -m 600 -o root -g wheel service/com.monokulo.push-relay.plist /Library/LaunchDaemons/
sudo nano /Library/LaunchDaemons/com.monokulo.push-relay.plist   # the secrets, in place of REPLACE
sudo -u nobody MONOKULO_RELAY_HANDLE_KEY="…" /usr/local/bin/monokulo-push-relay \
  --options "$dir/relay.toml" --store-path "$dir/push-relay/push-relay.db" --check
sudo launchctl bootstrap system /Library/LaunchDaemons/com.monokulo.push-relay.plist
```

The plist holds the secrets, so it is root's alone; delete the lines of a
secret you don't use. Its log is `$dir/push-relay/push-relay.log`. Stop it
with `sudo launchctl bootout system/com.monokulo.push-relay`.

**Windows.** From an administrator PowerShell in the unzipped folder:

```powershell
powershell -ExecutionPolicy Bypass -File service\install-windows-task.ps1
```

It installs the relay to `C:\Program Files\monokulo\monokulo-push-relay\`,
keeps its options, secrets (one file per variable, the handle key made for
you), handles and log in `C:\ProgramData\monokulo\monokulo-push-relay\`, and
registers a task that starts it at boot as `LOCAL SERVICE` and again when
it stops with an error. A startup task rather than a Windows service, as the
relay speaks no service control protocol. `-Uninstall` removes it and keeps
the data.

## Building it

From a checkout of the monokulo repository:
`cargo build --release -p push-relay --bin monokulo-push-relay`.
