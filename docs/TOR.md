# Running monokulo as a Tor onion service

Monokulo can serve its checkout, embed library and dashboard over Tor, on its
own or alongside a clearnet address. The engine stays private either way: it
listens on loopback and only monokulo talks to it (`docs/DESIGN.md` §4).

This page explains the `torrc` lines in [`deploy/tor/torrc.snippet`](../deploy/tor/torrc.snippet)
and how they fit with monokulo's own abuse protection
([`docs/ABUSE_PROTECTION.md`](ABUSE_PROTECTION.md)).

## Why an onion service needs its own listener

Tor delivers every onion visitor from the local tor process, so to an ordinary
listener they all come from `127.0.0.1`: one busy visitor would use up the
limit for everyone, and an attacker could lock out every real customer.

Tor can tell monokulo which circuit each connection arrived on. Tor Browser
keeps one circuit per site per browsing session, so a circuit behaves like one
visitor. Monokulo therefore has a second, loopback-only listener
(`abuse.onion_listener`, e.g. `127.0.0.1:8082`) that:

- requires every connection to start with a PROXY protocol v1 header, which is
  how tor passes the circuit on (`HiddenServiceExportCircuitID haproxy`), and
  closes any connection without one;
- identifies the client by the circuit id (the last 32 bits of the source
  address tor writes into the header, inside `fc00::/16`);
- refuses to start on anything but a loopback address, because it believes the
  header, and only the local tor may be allowed to send it.

Monokulo's ordinary listener (`127.0.0.1:8081`) never accepts a PROXY header.

## Checking your tor

```sh
tor --version        # 0.4.8 or newer
tor --list-modules   # must include "pow: yes"
```

Proof-of-work defences need tor 0.4.8 or newer built with the `pow` module.
Most distribution packages and the Tor Project's own packages include it. If
`pow: no`, tor refuses to start with `HiddenServicePoWDefensesEnabled 1`.

After starting, check tor accepted the settings, e.g. through the control port
(`GETCONF HiddenServicePoWDefensesEnabled`) or tor's log. The Rust test
`crates/monokulo/tests/e2e_tor.rs` does exactly that.

## The lines, one by one

| Line | What it does |
|---|---|
| `HiddenServiceDir /var/lib/tor/monokulo/` | Where tor keeps the service's keys; `hostname` in it is your `.onion` address. Back it up: losing it loses the address. |
| `HiddenServiceVersion 3` | v3 onion services (the only kind still supported). |
| `HiddenServicePort 80 127.0.0.1:8082` | Visitors' port 80 goes to monokulo's onion listener. It must be the onion listener, not monokulo's ordinary port. |
| `HiddenServiceExportCircuitID haproxy` | Tor prefixes each connection with a PROXY v1 header naming the circuit, so monokulo can tell visitors apart. |
| `HiddenServicePoWDefensesEnabled 1` | Under load, clients must solve a puzzle before tor sets up their connection; the effort rises with the attack. Tor Browser solves it automatically, without JavaScript. Costs nothing when there's no attack. |
| `HiddenServicePoWQueueRate 50`, `HiddenServicePoWQueueBurst 250` | How fast tor hands queued introductions on to be served, highest effort first. Lower means less load reaches monokulo under attack; too low slows legitimate visitors during one. |
| `HiddenServiceEnableIntroDoSDefense 1` | Asks the introduction points to rate-limit introductions before they reach your machine. |
| `HiddenServiceEnableIntroDoSRatePerSec 25`, `...BurstPerSec 200` | The rate and burst for that limit (per introduction point). |
| `HiddenServiceMaxStreams 64` | At most 64 simultaneous connections per circuit: plenty for a real visitor (pages, API calls, up to 16 live-update streams per store), and a hard ceiling for one abusive circuit. |
| `HiddenServiceMaxStreamsCloseCircuit 1` | A circuit that asks for more is closed rather than just refused, so it has to build a new circuit (and solve PoW again under attack). |

## Monokulo settings

- `abuse.onion_listener` (`MONOKULO_ABUSE_ONION_LISTENER`): `127.0.0.1:8082`.
  Read at startup; restart monokulo after changing it.
- `public_url` (`MONOKULO_PUBLIC_URL`): `http://<your address>.onion` if the
  onion address is the one plugins and customers should use.
- The limits (`abuse.soft_per_min`, `abuse.hard_per_min`, `abuse.stream_cap`,
  ...) then apply per circuit. See `docs/ABUSE_PROTECTION.md`.

## Testing it

`crates/monokulo/tests/e2e_tor.rs` runs a real tor with this snippet's lines in
front of monokulo and checks per-circuit identities, limits, challenges and the
stream cap. It needs the live Tor network, so it's ignored by default:

```sh
cargo test -p monokulo --test e2e_tor -- --ignored --nocapture
```

See `e2e/README.md` for details.
