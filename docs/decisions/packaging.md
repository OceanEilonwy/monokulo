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

## OpenWrt packages (2026-10-29)

21. **One package per service, its LuCI page inside, exactly as
    `openwrt/monokulo` is** (the owner's brief): `monokulo-push-relay` and
    `monokulo-bell`, each with its binary, a procd init script, its UCI
    config, its keep file, its LuCI view, menu and ACL (the last two named
    `luci-app-<package>.json`), and its page's backend. What the two init
    scripts and backends share is written once, in `openwrt/common/`
    (`service.sh`, `rpcd.uc`), and each package installs its own copy
    (`/usr/share/<package>/service.sh`, `/usr/share/rpcd/ucode/<package>`),
    so neither package needs the other. Otherwise: a copy per package,
    which would drift.
22. **The three binaries are built in one cargo invocation and one SDK
    run, into one feed: one `packages.adb` per release and architecture,
    signed with the one key (`openwrt/keys/monokulo.pem`).** The
    `openwrt-repo-<release>-<arch>` artifacts, `pages fetch` and pages.yml
    need no change: the feed the site already publishes now holds three
    packages. The router page's "Package files" lists each, and the site
    build fails if a repository lacks one (`install::PACKAGES`), so a feed
    without the services can't be published by mistake.
23. **The packages build for aarch64_cortex-a53 (the Flint 2) and x86_64,
    monokulo's included** (`DEPENDS:=@(aarch64||x86_64)`), as two
    openwrt.yml matrix entries on the 25.12.5 SDK images. x86_64 is the
    one CI boots (24); aarch64's proof is the static-link check and the
    owner's router, as there is no `aarch64_cortex-a53` rootfs image.
24. **CI boots OpenWrt to test the packages**: openwrt.yml's `test` job
    (after the build, so release.yml's Pages deploy waits for it too) runs
    `scripts/test-openwrt.sh` in `openwrt/rootfs:x86-64-25.12.5` with
    procd as PID 1 (`--privileged`). It trusts the key that signed the
    build (`dist/throwaway.pem` when no real key did), installs each
    package by name from the signed repository as the router page says,
    and covers: the generated secret made on install, root's alone; the
    firewall rule made off; the service refusing to start without its
    public address and saying why; UCI settings written into the options
    file; the start as its own user; the status listener; LuCI's backend
    through uhttpd's JSON-RPC under a login granted the page's ACL alone
    (and that ACL refusing procd's `service list` and file reads); a
    secret set, refused (the generated one, a path) and cleared; a bad
    value refused by `--check` in the binary's words, both on a reload
    (still running, same pid) and a restart (not started); a value TOML
    would choke on reaching the binary as written; an upgrade (`apk fix
    --reinstall`) keeping settings and secrets; the relay refusing to run
    beside monokulo's own; the sysupgrade backup list; and removal. monokulo
    gets install, start, upgrade and removal in the same run.
25. **A throwaway key is made by the build when there is no real one**,
    and every package and the index are signed with it, its public half in
    `dist/throwaway.pem`: a fork's pull request builds a repository the
    rootfs test can verify (no `--allow-untrusted` anywhere). Before, an
    unsigned build left the packages unsigned.
26. **Secrets are a root-only folder `/etc/<package>/secrets/`, one file
    per variable** (the APNs `.p8` and the FCM JSON are multi-line files),
    0700/0600, passed to the binary in its environment by procd and by
    `--check`. The generated one (the relay's handle key, the Bell's VAPID
    key: 32 random bytes as hex) is made in `postinst`, so it exists to
    back up from the start, and never replaced.
27. **The options file is written from UCI at every start, into
    `/var/etc/<package>/` (memory), as dotted keys with every value a TOML
    string.** It holds no secret and is rebuilt each time, so nothing is
    written to flash; dotted keys (`server.public_uri = "…"`) need no table
    headers, so the order of writing can't make a table twice; a string
    the binary parses makes a wrong value the binary's own error. Only
    options that are set are written, so the binary's defaults rule and UCI
    never repeats them. A value with a control character is refused by the
    renderer (none is valid).
28. **A bad setting never stops a running service, and never starts a
    stopped one.** Each start renders to a new file and runs `--check` on
    it; on success it replaces the file the service runs on, on failure the
    reason goes to `/tmp/<package>.check` (LuCI shows it) and a running
    service is re-registered on its last good file (procd leaves it
    running), while a stopped one stays stopped. The options file's hash is
    in the instance's data, so new settings restart it and a refused reload
    doesn't. Otherwise: the plan's "check then restart" in the page alone,
    which a `uci commit` by hand or a reload trigger would bypass.
29. **Save & Apply on each page commits UCI, runs the backend's `check`
    (the init script's `check`: the same renderer and `--check`), and only
    then reloads the service**; refused, it shows the binary's output and
    leaves the service on its earlier settings (28). The page's ACL grants
    `uci commit` for its own config to do that.
30. **The page's backend is an rpcd ucode plugin, not a shell one**: rpcd's
    ucode support ships with LuCI (luci-base needs it), and ucode builds
    the JSON (the check's output, the service's own status) without shell
    quoting. Methods: `status`, `check`, `set_secret`, `clear_secret`; it
    reads procd's state itself, so the page's ACL grants no `service list`,
    whose instances hold each secret in their environment. It never sets or
    clears the generated secret (restoring one is a file copied in by
    hand, as the page says) and takes only the secrets' own names.
31. **The standalone relay refuses to start while monokulo on the same
    router has `relay.enabled`** (UCI says monokulo is on, and an awk over
    its options file finds `enabled = true` in `[relay]` itself), whatever
    the ports: two relays on one router is a mistake. The reason is in the
    page's Status row. The relay's reload trigger watches monokulo's UCI
    too.
32. **Listen: the relay on the LAN by default, the Bell on this router
    only** (`listen` is lan, all or loopback, with `port`), as their own
    defaults lean; the status listeners are fixed at 127.0.0.1:8446 and
    127.0.0.1:8447, where LuCI's backend reads them.
33. **A firewall rule per package, `Allow-<package>` from wan to its port,
    made off** by `/etc/uci-defaults/90-<package>` and removed with the
    package. Neither binary serves TLS (12), so each page and the docs give
    the TLS proxy on the router instead (Caddy, with nginx as the other
    recipe); the packages install no proxy.
34. **Removal keeps the data folder and the secrets**, the operator's, as
    monokulo's package does.
35. **The relay's apps are anonymous UCI sections with a `name` option**:
    an app's name may hold a hyphen, which a UCI section name can't.
36. **A test in each crate holds its package to its settings**
    (`crates/<crate>/tests/openwrt.rs`): every setting the options file
    takes is written by the init script from a UCI option the LuCI page
    shows, the secrets of the init script, the page and the backend are
    the binary's, and the default port and status listener are the
    binary's own. A new setting fails CI until the package has it.
37. **monokulo's own LuCI page no longer reads procd's `service list`**,
    found while testing 30: procd's list (not only its verbose one) shows
    each instance's environment, so the page fetched monokulo's encryption
    key into the browser, over LuCI's plain http on most routers. The
    package now installs the same backend for `status` alone (object
    `monokulo`), the page reads whether it runs and which build from it,
    and its ACL grants that instead of `service list` and the version
    file. The rootfs test holds all three pages' ACLs to refusing `service
    list`.
