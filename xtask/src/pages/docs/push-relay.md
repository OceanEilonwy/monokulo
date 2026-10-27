# The push relay

A phone that isn't running your app hears of a sale from a push. On
Android with a [UnifiedPush](https://unifiedpush.org/) distributor, a
Monokulo pushes straight to the distributor. An iPhone, or an Android phone
with only Google's services, takes pushes from APNs or FCM alone, which
take them only from whoever holds the app's Apple or Google keys. The push
relay is that holder: a [Web Push](https://www.rfc-editor.org/rfc/rfc8030)
service whose endpoints lead to APNs and FCM, so a Monokulo pushes to an
iPhone the way it pushes anywhere else.

The relay learns as little as it can. A notification is encrypted to the
device ([RFC 8291](https://www.rfc-editor.org/rfc/rfc8291)) and the relay
hands the ciphertext on; only a Live Activity's state is plain JSON, as iOS
applies it with no code of the app's to decrypt it. It keeps no push, no
store and nothing linking a device's handles, and logs no body, token or
endpoint.

## 1. Register

The app registers its device token with the relay, naming the sender it
wants pushes from by that sender's VAPID public key (a Monokulo's is its
`/api/v1/push-targets`' `vapid_public_key`, see
[Connected apps](../apps/#6-get-pushes)):

```
POST https://relay.example.org/v1/register
Content-Type: application/json

{ "platform": "apns", "token": "7f3k…", "app": "monokular",
  "vapid_public_key": "BOz2…" }
```

```json
{ "endpoint": "https://relay.example.org/v1/push/ph_7f3ab0c4d51e4e8a9b20c6d1e2f30415",
  "expires_at": 1801234567 }
```

- `platform`: `apns` (notifications, encrypted), `apns-liveactivity` (a
  Live Activity's updates, by its push token), `apns-pushtostart` (starting
  a Live Activity, iOS 17.2), or `fcm`.
- `token`: the APNs token in hex (either case), or the FCM registration token.
- `app`: the app's name, as the relay's operator set it up
  (`handles.apps`).

Hand `endpoint` to the sender as your subscription's, with `kind`
`encrypted` and the subscription's own keys, or `live-activity` for a Live
Activity. It answers `201 Created`. Registering the same token, platform,
app and key again gives the same endpoint, its expiry pushed back: do so
whenever the app starts or the platform hands it a new token. A handle
lasts 90 days after its last registration or its last push, and goes as
soon as APNs or FCM says its token is gone. A device with two senders (two
stores, or a store and a Monokular Bell) registers once for each and gets
two endpoints, which take pushes only from their own sender.

## 2. Push

A sender pushes to the endpoint as to any Web Push endpoint: `TTL`,
`Urgency` and `Topic` headers, signed with VAPID for the relay's own
address, its body `Content-Encoding: aes128gcm` (to `apns` and `fcm`
handles) or `Content-Type: application/json` (to Live Activity handles).
The relay answers `201 Created` once APNs or FCM took it.

| Handle | Handed on as |
| --- | --- |
| `apns` | An alert, `mutable-content` on, titled "New sale" until the app's Notification Service Extension decrypts the ciphertext it carries as `aes128gcm` (base64url) and writes its own. Priority 10 for a high-urgency push, 5 otherwise; expires after the push's TTL; collapsed by its topic. |
| `apns-liveactivity` | The JSON as the activity's `content-state`, read for `phase` (`waiting`, `seen`, `confirming`, `paid` or `expired`), `expiresAt` (Unix seconds: the activity's stale date), and `amount`; the rest goes to APNs as it came. Every push is an `update`: `paid` and `expired` come with an alert ("Paid · $24.50", "No payment yet"), and the relay never ends the activity, which the app idles or ends itself. |
| `apns-pushtostart` | A new activity: the JSON is `{"attributes-type", "attributes", "content-state"}`, handed on as they are, with an alert. |
| `fcm` | A data message carrying the ciphertext as `aes128gcm`, priority `HIGH` for a high-urgency push and `NORMAL` otherwise, kept for the TTL (at most 28 days), collapsed by its topic. |

APNs and FCM carry 4096 bytes, so a push whose ciphertext is over about
3000 bytes is refused as too large once base64 and the envelope are around
it.

## Problems

Errors are `application/problem+json` ([RFC 9457](https://www.rfc-editor.org/rfc/rfc9457)).

### `invalid-request` · Invalid request

`400`. The body isn't what the route takes: not JSON, a field missing or
unknown, a token that isn't one for its platform, a VAPID key that isn't a
P-256 point; a push without `TTL`, or with a body its handle doesn't take
(JSON to `apns` or `fcm`, ciphertext to a Live Activity). `detail` says
which.

### `unknown-app` · Unknown app

`400`. The relay serves no app by that name.

### `unsupported-platform` · Platform not served

`400`. The relay serves the app, but has no key for that platform.

### `unknown-handle` · Unknown handle

`404`. No such endpoint: never made, unused for its lifetime, or its token
was gone. A sender drops the subscription; the app registers again.

### `unauthorized` · Not signed by the handle's sender

`401`. The push's VAPID signature isn't the key the handle was registered
with, isn't for this relay's address, or has expired.

### `payload-too-large` · Too large

`413`. More than 4096 bytes, or more than APNs or FCM carries once wrapped
for them.

### `rate-limited` · Too many requests

`429`. Too many registrations from one address, or pushes from one sender
or to one handle, this minute. Wait `Retry-After` seconds.

### `upstream-unavailable` · APNs or FCM unavailable

`503`. APNs or FCM is down, busy or limiting the relay. Try again after
`Retry-After` seconds.

### `upstream-refused` · Refused by APNs or FCM

`502`. APNs or FCM refused the push for a reason that is the relay's own
(its keys or settings); its log says which.

### `internal` · Relay error

`500`. Something went wrong on the relay. Try again in a moment.

## Run one

A relay needs the address it is reached at, over https, and the keys of
the apps it serves. On its own:

```sh
monokulo-push-relay --init     # writes ~/.config/monokulo/relay.toml
MONOKULO_RELAY_APNS_KEY="$(cat AuthKey_ABCDE12345.p8)" \
MONOKULO_RELAY_FCM_CREDENTIALS="$(cat service-account.json)" \
  monokulo-push-relay
```

with, in `relay.toml`:

```toml
[server]
listen = "0.0.0.0:8444"
public_uri = "https://relay.example.org"

[handles.apps.monokular]
apns_topic = "com.example.monokular"
fcm_project_id = "monokular-12345"

[apns]
team_id = "ABCDE12345"
key_id = "FGHIJ67890"
```

Or inside Monokulo, with the same settings under `[relay.*]` of its own
options file (`--relay-…` on its command line) and `enabled = true` in
`[relay]`: it listens on its own port, keeps its handles in its own
`push-relay.db` beside Monokulo's database, and is checked at every start
even while it is off. The APNs key and the service account come from the
environment only, in both cases. `monokulo-push-relay --help` describes
every setting.
