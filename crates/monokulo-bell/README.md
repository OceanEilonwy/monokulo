# monokulo-bell

The Monokular Bell: it turns a point of sale's payment webhooks (Square's,
Clover's, or both) into Web Push dings that wake the
[Monokular](https://github.com/OceanEilonwy/monokular) tills registered at
that location, so a till shows a Monero payment the moment it lands. It
keeps the tills' registrations, never a payment. It is its own program,
never part of monokulo. Everything about it, the API included:
<https://docs.monokulo.com/bell/>.

## What is in the zip

| File | What it is |
| --- | --- |
| `monokulo-bell` (`.exe` on Windows) | the Bell |
| `bell.toml` | a starter options file: every setting with what it is for and its default, commented out |
| `service/monokulo-bell.service` | a systemd unit (Linux) |
| `service/com.monokulo.bell.plist` | a launchd daemon (macOS) |
| `service/install-windows-task.ps1` | installs it as a Windows startup task |

The zip's `.sha256` beside it on the release page is its SHA-256.

## Run it

A Bell serves Square, Clover, or both, each through an application of its
own (the one the tills sign in through) whose webhooks come to it, and needs
an https address they can reach. Set it in `bell.toml` (`--init` writes a
fresh one to `~/.config/monokulo/bell.toml`); a point of sale is on when its
`application_id` is set:

```toml
[bell]
public_uri = "https://bell.example.org"

[bell.square]
application_id = "sq0idp-…"

[bell.clover]
application_id = "ABCDEFGHIJKLM"
environment = "north_america"
```

The secrets come from the environment only, never the options file or the
command line; each point of sale's is set when, and only when, it is on:

| Variable | |
| --- | --- |
| `MONOKULO_BELL_VAPID_KEY` | required: 64 hex characters (`openssl rand -hex 32`); keep it, as the tills' subscriptions are made for its public key |
| `MONOKULO_BELL_SQUARE_WEBHOOK_KEY` | Square: the webhook subscription's signature key |
| `MONOKULO_BELL_CLOVER_AUTH_CODE` | Clover: the app's Auth Code |

The names from when it was `monokular-bell` (`MONOKULAR_BELL_*`) are still
read for one release, each with a warning at start.

```sh
(umask 077; openssl rand -hex 32 > vapid.key)   # once; keep it
export MONOKULO_BELL_VAPID_KEY="$(cat vapid.key)"
export MONOKULO_BELL_SQUARE_WEBHOOK_KEY=…
./monokulo-bell --options bell.toml --check   # what it would run on, then OK; or why not
./monokulo-bell --options bell.toml
```

It listens on `127.0.0.1:8445` (`bell.listen`) and keeps its registrations
in `~/.local/share/monokulo/bell.db` (`bell.store_path`). Any setting can
be given on the command line too (`--help` lists them); anything invalid or
missing, or no point of sale on, stops it before it listens, with the
reason. Setting up Square's application and Clover's app is on the
[docs page](https://docs.monokulo.com/bell/#run-one).

**TLS:** the Bell serves plain HTTP, on this machine by default. Put an
https reverse proxy in front on the same machine (Caddy, nginx), serving
`bell.public_uri`; a proxy on this machine is trusted for
`X-Forwarded-For` by default. The Bell itself uses TLS only to reach
Square, Clover and the tills' push services, trusting the machine's
certificate store.

**Status:** `curl -s http://127.0.0.1:8447/status` (`status.listen`, this
machine only) says what it is doing as JSON: its address, the registrations
it holds, which points of sale are ready, and the last ding it sent.

The binaries aren't signed. On macOS clear the quarantine of a downloaded
one once (`xattr -d com.apple.quarantine monokulo-bell`); on Windows
SmartScreen may ask first (More info > Run anyway).

## Run it as a service

Each file in `service/` says how to install it in its first lines. All
three run `--options` and the store path at a fixed place, so the Bell
needs no home directory.

**Linux (systemd).** `service/monokulo-bell.service` runs it as a user
systemd makes for it, its registrations in `/var/lib/monokulo/bell/`, its
options in `/etc/monokulo/bell.toml` and its secrets in the root-only
`/etc/monokulo/monokulo-bell.env`, checking the settings (`--check`) before
each start. `journalctl -u monokulo-bell` is its log.

**macOS (launchd).** `service/com.monokulo.bell.plist` runs it at boot as
`nobody`:

```sh
dir="/Library/Application Support/monokulo"
sudo mkdir -p /usr/local/bin "$dir/bell"
sudo install -m 755 monokulo-bell /usr/local/bin/
sudo xattr -d com.apple.quarantine /usr/local/bin/monokulo-bell
sudo install -m 644 bell.toml "$dir/bell.toml"   # then set bell.public_uri and a point of sale
sudo chown nobody "$dir/bell" && sudo chmod 700 "$dir/bell"
sudo install -m 600 -o root -g wheel service/com.monokulo.bell.plist /Library/LaunchDaemons/
sudo nano /Library/LaunchDaemons/com.monokulo.bell.plist   # the secrets, in place of REPLACE
sudo -u nobody MONOKULO_BELL_VAPID_KEY="…" MONOKULO_BELL_SQUARE_WEBHOOK_KEY="…" /usr/local/bin/monokulo-bell \
  --options "$dir/bell.toml" --bell-store-path "$dir/bell/bell.db" --check
sudo launchctl bootstrap system /Library/LaunchDaemons/com.monokulo.bell.plist
```

The plist holds the secrets, so it is root's alone; delete the lines of a
point of sale that is off. Its log is `$dir/bell/bell.log`. Stop it with
`sudo launchctl bootout system/com.monokulo.bell`.

**Windows.** From an administrator PowerShell in the unzipped folder:

```powershell
powershell -ExecutionPolicy Bypass -File service\install-windows-task.ps1
```

It installs the Bell to `C:\Program Files\monokulo\monokulo-bell\`, keeps
its options, secrets (one file per variable, the VAPID key made for you),
registrations and log in `C:\ProgramData\monokulo\monokulo-bell\`, and
registers a task that starts it at boot as `LOCAL SERVICE` and again when
it stops with an error. A startup task rather than a Windows service, as the
Bell speaks no service control protocol. `-Uninstall` removes it and keeps
the data.

## Building it

From a checkout of the monokulo repository:
`cargo build --release -p monokulo-bell --bin monokulo-bell`.
