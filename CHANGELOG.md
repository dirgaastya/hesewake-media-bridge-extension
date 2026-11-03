# Changelog

## [Unreleased]

- The addresses told to phones follow the PC: every 5 s the bridge looks again, and when a VPN or another network comes or goes it announces the new addresses (and redraws the pairing code if it is on show). Before, they were read once at start, so a VPN that was up at that moment kept being announced after it went down, and phones could not find the PC.
- OpenVPN (including its DCO adapter), NordLynx and Hamachi adapters are recognised as virtual, so their addresses are not told to phones.

## [0.1.1] - 2026-11-02

- The phone keeps one entry when it pairs again: the bridge knows it by the `device_id` the phone sends, keeps its name and place, and kills the old token.
- The port is kept between runs, so the address a phone remembers stays good after the bridge restarts.
- Phones are told only the addresses of real network adapters; a VPN or virtual adapter (NetBird, WireGuard, Tailscale, VM) is never advertised.
- Command ids are lower-case UUIDs, as the phone requires (before, every control button closed the connection).
- The "connected" toast no longer repeats when a phone reconnects.
- The log tells where a connection came from and why pairing was refused.

## [0.1.0] - 2026-11-02

First version.

- Extension for Hesewake Notch 0.13.0: a settings panel inside the notch (pair a phone with a QR code or the six digits, rename or forget phones, see what plays with its picture).
- Phone connection: TLS with a pinned certificate, pairing with a code both sides approve, tokens kept as hashes, DNS-SD advertisement, optional features agreed at connect time (`artwork_delta_v1`, `forget_v1`, the phone's own name).
- A Windows media session ("Media from phone") from the phone's snapshots, with the delay measured and added to the position, and play, pause, next, previous and seek sent back as commands.
- A log of why a connection ended, without any song data.
