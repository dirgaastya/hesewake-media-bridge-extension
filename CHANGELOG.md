# Changelog

## [0.1.0] - 2026-11-02

First version.

- Extension for Hesewake Notch 0.13.0: a settings panel inside the notch (pair a phone with a QR code or the six digits, rename or forget phones, see what plays with its picture).
- Phone connection: TLS with a pinned certificate, pairing with a code both sides approve, tokens kept as hashes, DNS-SD advertisement, optional features agreed at connect time (`artwork_delta_v1`, `forget_v1`, the phone's own name).
- A Windows media session ("Media from phone") from the phone's snapshots, with the delay measured and added to the position, and play, pause, next, previous and seek sent back as commands.
- A log of why a connection ended, without any song data.
