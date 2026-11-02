# Wire protocol between the phone app and the bridge (v1)

The phone is the client, the PC (the bridge) is the server. This describes what is on the wire; it is the contract both
projects test against.

## Finding the PC

- **DNS-SD**: the bridge advertises `_hesewake-media._tcp` with its port while it runs.
- **QR code**: `hesewake-media:1:` followed by the base64url (no padding) of `{"host": "...", "port": N, "fingerprint": "<hex sha-256>"}`.
  The phone connects to that address and refuses a certificate whose fingerprint differs.

## Transport

TLS 1.2 or 1.3 over TCP. The server certificate is self-signed; the phone **pins its SHA-256 fingerprint** (from the QR
code, or the one it saw when it paired). The first frame must come within 5 seconds.

Frame: a 4-byte **big-endian** length, then that many bytes of UTF-8 JSON (1 to 1 048 576). Every message is a JSON object
with `"v": 1` and `"type"`. Strings are at most 4096 bytes. A malformed frame, an unknown `type` or a message that does not
fit the state ends the connection.

Encodings: nonces and tokens are 32 random bytes in **base64url without padding**. Artwork is **standard base64 with
padding**.

## Pairing

Only while the user pressed "Pair a phone" on the PC (two minutes); five failures lock it for a minute.

1. phone: `pair_hello` `{client_nonce, device_name?, device_id?}`
2. PC: `pair_challenge` `{server_nonce}`
3. Both compute the code: the first 4 bytes (big-endian) of
   `SHA-256("hesewake-media-pair-v1" || fingerprint || client_nonce || server_nonce)`, modulo 1 000 000, six digits. Each
   side shows it; the user approves on both.
4. each: `pair_decision` `{approved}`
5. PC, if both approved: `pair_result` `{approved: true, token, capabilities}`. The phone keeps the token. The PC keeps
   only its SHA-256.

`device_name` (optional text) becomes the phone's name on the PC unless the user has renamed it.

`device_id` (optional, 8 to 64 characters of letters, digits, `-` and `_`) is the phone's own id for itself and must be **the same
every time** (the reference app sends a hash of its Android ID). It is how a phone that pairs again is known: the PC keeps one entry
for it (with its name and place), gives it the new token and kills the old one, instead of adding a second phone. A phone that sends
none, or a bad one, is a new entry every time.

## Signing in

phone: `auth` `{token, device_name?, device_id?}` → PC: `auth_result` `{approved, capabilities?}`. A wrong token is refused after a short
delay. A phone that signs in replaces the connection it had. A phone that was paired before it sent a `device_id` gets the one it signs in with.

`capabilities` is a list of optional features the PC supports; the phone uses only what it is told:

- `artwork_delta_v1`: a snapshot may say `"artwork_same": true` instead of carrying the picture again.
- `forget_v1`: the phone may send `forget`.

## While connected

- phone → PC `snapshot`, about once a second: `seq` (grows with every snapshot, starts at 0 on each connection), `active`,
  `source` (Android package), `title`, `artist`, `album`, `artwork_mime` (`image/jpeg` or `image/png`),
  `artwork_base64` (at most 262 144 bytes once decoded), `position_ms`, `duration_ms`, `playing`, `speed` (0 to 4), `actions`
  (any of `play pause next previous seek`).
  - The first snapshot of a connection carries the picture in full. With `artwork_delta_v1`, a later one may leave
    `artwork_mime`/`artwork_base64` out and set `"artwork_same": true`: the picture is the last one of this connection. With
    neither, the song has no picture. `active: false` clears it. A picture never carries over to another connection.
  - `position_ms` is the position at the moment of sending; the PC adds the measured one-way delay.
- PC → phone `ping` `{id}` every 10 seconds (and a few quickly after connecting, to measure the delay); the phone answers
  `pong` `{id}`. The phone may ping too. The PC drops a phone that sends nothing for 35 seconds.
- PC → phone `command` `{id, action, position_ms?}`: `id` is a **lower-case UUID** (the phone refuses anything else and
  closes the connection); `action` is one of `play pause next previous seek` (`position_ms` only for `seek`).
- phone → PC `ack` `{id, accepted, reason?}`. An action the player does not support is `accepted: false`.
- phone → PC `forget` (only with `forget_v1`): the PC revokes this phone's token, saves, answers `forget_result`
  `{approved}` and closes. If it cannot save, it answers `approved: false` and changes nothing.

## Versioning

`v` stays 1 while changes are additive: new optional fields, new `capabilities`. A side ignores fields it does not know.
