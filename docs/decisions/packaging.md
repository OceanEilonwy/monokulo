# Decisions: packaging the push relay and the Bell

Made on 2026-10-29 without the owner to ask, while giving
`monokulo-push-relay` and `monokulo-bell` what a service manager and a
router need: `--check`, a status for this machine, release zips and
OpenWrt packages. Each says what was chosen, why, and what else was
possible, so any of them can be reversed knowingly. The owner's plan (the
Bell moved into this repository, one OpenWrt package per service with its
LuCI page inside, five release targets) is the brief, not re-decided here.
The Bell's move is in `docs/decisions/bell.md` (59 on); the relay's own
entries in `docs/decisions/push.md` point here (162).

## `--check` and the status (2026-10-29)

1. **`--check` is the first half of a start.** `Relay::check` and
   `Bell::check` hold every check a start makes that needs no file and no
   network: the settings loaded and checked together, the public address
   and the required secrets set, at least one point of sale on (the Bell),
   and every client built (APNs, FCM, Square, Clover, the push sender) and
   the ding topic parsed. `start` begins by calling `check` and runs on
   what it returns (the clients included), then opens the database and
   binds, so there is one validation path and `--check` can't pass a
   setting a start would refuse. Otherwise: a separate validator, which
   would drift from `start` the first time a check is added to one.
2. **`--check` touches nothing on disk or the network.** It doesn't open
   or create the database or its directory, and binds no port (a test
   holds both ports taken while it passes), so root (LuCI's save, an init
   script, systemd's `ExecStartPre`) can run it for the service's user
   without leaving a root-owned file behind. A port already taken is
   therefore found by the start, not the check; trying a bind would make
   the check depend on whether the service is running, which is exactly
   when it is run before a restart.
3. **`--check` is added by `live_settings::cli::with_check`, not by
   `with_settings`**, so the engine and monokulo, which don't implement it,
   don't offer a flag they would ignore (`Start::check` is false for them,
   read with `try_get_one`). It conflicts with `--init`.
4. **One output format.** Success: what the service would run on, a fact a
   line, then `OK`, on stdout, exit 0. The Bell puts a secret still under
   its `MONOKULAR_BELL_*` name first, as a `warning: …` line (the same
   words a start logs). Failure: the words a start would stop with
   (`the push relay didn't start: …`, `the Bell didn't start: …`, or the
   settings' own error), on stderr, exit 1, nothing on stdout; both binary
   test tables run every case with and without `--check` and expect the
   same text. A page (LuCI) shows the text as it is. No fact names a
   secret: the VAPID public key and the Square application id are public
   (the tills hold them).
5. **The status is a listener of its own, `status.listen`, never a route
   on the service's listener.** The service's listener is published by
   the TLS proxy in front of it; a `/status` route there would be
   published with it. The setting's type is `live_settings::LoopbackAddr`,
   which refuses any address that isn't loopback as it is parsed (the
   options file, the command line), so it can't be bound to the network
   by mistake. Defaults: `127.0.0.1:8446` for the relay, `127.0.0.1:8447`
   for the Bell (their listeners are 8444 and 8445); empty turns it off.
   It needs no credential: it is local by construction (and see 6).
6. **The status refuses a request whose `Host` isn't this machine**
   (`localhost` or a loopback address, with or without a port; 403). A web
   page could otherwise read it by pointing a name of its own at
   `127.0.0.1` (DNS rebinding): the browser would send that name as the
   host. A request with no `Host` (HTTP/1.0 on the machine) is taken.
   Otherwise: a token, which LuCI and every curl would then need, for a
   report that holds nothing secret.
7. **Inside monokulo there is no relay status listener**: `status.listen`
   is in the relay's `STANDALONE_ONLY` with its logging, so `[relay.status]`
   and `--relay-status-listen` are unknown there and stop monokulo. The
   admin page's Push relay card is the embedded relay's status.
8. **The status carries counts, times and readiness only.** The relay:
   service, version, commit, start time, the bound listener, the public
   address, its apps by name, handles stored, APNs and FCM `ready` or
   `not-configured`, and the last push handed on (`sent`, `gone`,
   `refused`, `unavailable`, `too-large`) with its time. The Bell: the same
   head, registrations stored (every point of sale's, one count), Square
   and Clover `ready` or `off`, and the last ding (`sent`, `gone`,
   `failed`; a push service limiting the Bell is `failed`, and a ding the
   merchant's share dropped was never sent, so it isn't one). Never a
   token, endpoint, handle id, merchant, location, payment, body or key;
   the over-HTTP tests look for each in the JSON. A count the database
   can't give is `null` and logged, not an error page. The last event is
   in memory (`service_runtime::status::Last`): a restart forgets it,
   which `started_at` says.
9. **The shared pieces live where their kind does.** `LoopbackAddr` is a
   setting value type, beside `BindAddr` in `live-settings` (a
   `service-runtime` type couldn't implement `SettingValue` without that
   crate depending on `live-settings`). The listener (`router`, `serve`,
   the `Host` check), `Last` and the build's commit are
   `service_runtime::status`, behind a `status` feature that only the two
   standalone services turn on, so the engine doesn't build it.
10. **A status listener that can't bind stops the service**, as its main
    listener does ("failed to listen on 127.0.0.1:8446 for the status"):
    a router page reading a status that silently isn't there would be
    worse than a start that says why. It is bound after the main listener,
    and stops with the same signal and grace.
11. **`--version` names the commit** (`monokulo-push-relay 0.1.0
    (51dfea123)`, its first nine digits) when the build sets
    `MONOKULO_GIT_COMMIT`, as release.yml, `build-openwrt.sh` and the
    Dockerfile already do; the status gives the whole commit. A build
    without it says the version alone and `"commit": null`.
12. **TLS is a proxy's job: neither binary serves it.** Checked on
    51dfea123: both serve with axum's `http1` feature on a plain
    `TcpListener`; `rustls`, `tokio-rustls` and `hyper-rustls` reach them
    only through `reqwest`, as clients (the relay to APNs, FCM and Google's
    token endpoint; the Bell to Square, Clover and the push services), and
    nothing in either crate accepts TLS. Their public addresses
    (`server.public_uri`, `bell.public_uri`) must be https, so each needs a
    TLS-terminating reverse proxy in front: the docs pages say so, the
    router setup puts one there (Caddy or nginx, decided with the OpenWrt
    packages), and the Bell listens on loopback by default for it. The
    clients verify through `rustls-platform-verifier`, which reads the
    machine's certificate store, so the store must be installed
    (`ca-certificates`; `+ca-bundle` in the OpenWrt packages). Serving TLS
    in the binaries (rustls with certificate files, or ACME) was the
    alternative: it would mean certificate renewal inside each service,
    which a proxy already does for everything else on the host.

## Release zips and service files (2026-10-29)

13. **The two services' zips are one matrix job in release.yml,
    `services`** (`bin` × `target`, the runner by `include`): ten jobs
    beside `monokulo`'s five, after the same lint and coverage, named as
    monokulo's zips are (`<name>-<version>-<target>.zip`, `main` on main,
    `dev-<sha>` on a manual run of a branch) with a `.sha256` each.
    `github-release` and `latest-main` download `monokulo-push-relay-[0-9]*`
    and `monokulo-bell-[0-9]*` (`-main-*` on main), which the OpenWrt
    artifacts (`monokulo-openwrt-*`, `openwrt-repo-*`) can't match, and
    wait for the job; `scripts/release-notes.sh` lists the zips in a table
    of their own. x86_64 macOS is built on `macos-15-intel`, as monokulo's
    is, not cross-built on Apple silicon as the plan had it: neither
    service needs it for RandomX, but the checks below run the binary and
    its service, so they run on the computer the zip is for. Otherwise:
    a job per binary (twice the YAML for the same steps), or the services
    inside monokulo's job (one failure would hold back all three).
14. **The zip's starter options file is the binary's own `--init` on the
    runner that built it** (`relay.toml`, `bell.toml`), so the "check
    `--init` per runner" step and the shipped file are one thing, and the
    file always describes the settings of the binary beside it.
15. **The service files live in the crate, `crates/<crate>/service/`, and
    go in the zip as `service/`**, with the crate's `README.md` (what it
    is, its secrets, `--check`, the status, TLS, each service file). A
    unit test in each crate's `cli.rs`
    (`the_service_files_start_it_with_its_own_options_and_secrets`) parses
    the systemd `ExecStartPre`/`ExecStart`, the plist's
    `ProgramArguments` and the Windows script's store option with the
    binary's own parser, and holds the variables each file names to the
    binary's secrets exactly, so a renamed option or variable fails a test
    instead of a merchant's start.
16. **systemd: `DynamicUser`, `StateDirectory=monokulo/<push-relay|bell>`,
    the options in `/etc/monokulo/<relay|bell>.toml`, the secrets in a
    root-only `EnvironmentFile` (`/etc/monokulo/<name>.env`), and
    `ExecStartPre=… --check`.** The database path is given on the command
    line (`--store-path %S/monokulo/push-relay/push-relay.db`,
    `--bell-store-path …`), as a dynamic user has no home for the default.
    Secrets are written `VARIABLE='value'`: single quotes keep a value as
    it is over several lines (checked with systemd 261), so the APNs `.p8`
    and the FCM JSON go in whole. A setting `--check` refuses stops the
    start with its words in the journal, and `StartLimitBurst` keeps that
    from looping for ever. The usual sandboxing (`ProtectSystem=strict`,
    no capabilities, `MemoryDenyWriteExecute`: neither binary has a JIT).
    Otherwise: a fixed `monokulo` user (the installer would have to make
    it), or `LoadCredential` (it fails a start when an optional secret's
    file is missing, and the binaries read only the environment).
17. **launchd: a LaunchDaemon (`com.monokulo.<push-relay|bell>`) running
    as `nobody`, with the secrets as `EnvironmentVariables` in the plist
    (root's alone, 600)**, its data and log in
    `/Library/Application Support/monokulo/<push-relay|bell>/`,
    `KeepAlive` on an unsuccessful exit only. The placeholders read
    `REPLACE: …`, which the binary refuses, so a plist installed without
    its secrets stops with the reason rather than running half set up.
    The install steps are in the README, not the plist's comment: an XML
    comment can't hold `--`.
18. **Windows: `install-windows-task.ps1` registers a startup task as
    LOCAL SERVICE (decision 20 of the plan: the binaries speak no service
    control protocol) and is also the task's runner (`-Run`).** The
    program goes to `C:\Program Files\monokulo\<name>\`; the options file,
    the secrets, the database and the log to
    `C:\ProgramData\monokulo\<name>\`, which only administrators and
    SYSTEM change and LOCAL SERVICE reads (writing only its database and
    log). Each secret is a file named for its variable, as on the router
    (the `.p8` and the JSON are files already); the install makes the
    handle key or the VAPID key when there is none, as the OpenWrt
    packages do. The runner restarts the binary itself five seconds after
    an exit with an error (Task Scheduler's restart is for a task that
    fails, which a runner still running isn't) and stops a binary a killed
    runner left behind before starting it, so `Stop-ScheduledTask;
    Start-ScheduledTask` restarts it. `-Uninstall` removes the task and
    the program and keeps the data. Running it again upgrades in place.
19. **Every release runner installs and starts the service file from the
    folder it zipped** (`scripts/check-service-files.sh`): systemd on
    Linux, launchd on macOS, the task script on Windows, each as its
    header or the README says, with the least settings that start on
    loopback (a sandbox Square application for the Bell); it reads
    `/status` until it names the service, then stops and removes it and
    checks the status is gone (and on Windows that the generated secret
    was made and the task is gone). A zip whose service file can't start
    its binary isn't kept. Otherwise: checking the files' syntax only,
    which would miss a wrong path, user or permission.
20. **The zips are unsigned on macOS and Windows, as monokulo's are**; each
    README says how to clear macOS's quarantine (`xattr -d
    com.apple.quarantine`) and SmartScreen's question, as the release
    notes do for monokulo.
