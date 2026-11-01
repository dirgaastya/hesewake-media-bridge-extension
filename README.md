# Hesewake Media Bridge

Shows the media that plays on an Android phone in [Hesewake Notch](https://github.com/dirgaastya/hesewake-notch), over the
local network. The audio stays on the phone. The bridge publishes a Windows media session (SMTC) named "Media from phone",
so the notch's Music slide, its lyrics and the Windows media controls all work with it, and the buttons send play, pause,
next, previous and seek back to the phone.

It is an **extension**: a separate small program that the notch starts and talks to over a named pipe. It is not part of
the notch. How extensions work is in the notch repository, `docs/extensions.md`. The phone app is
`hesewake-bridge-android`; the wire protocol between the two is in [`docs/protocol.md`](docs/protocol.md).

## Requirements

- Windows 10 (1809) or 11, x64.
- Hesewake Notch **0.13.0 or newer** (the version that has extensions).
- An Android phone with the Hesewake Bridge app (Android 8.0 or newer), on the same Wi-Fi as the PC.

## Install

1. Run `HesewakeMediaBridge-Setup-<version>.exe`. It needs no administrator rights: it puts the program under
   `%LOCALAPPDATA%\Programs\Hesewake Media Bridge` and a manifest in `%APPDATA%\hesewake-notch\extensions\media-bridge`.
2. In the notch, open **Settings > Extensions**, press **Rescan**, and turn **Media Bridge** on.
3. Windows may ask to allow the program on private networks the first time it listens: allow it for **private** networks
   only. The phone cannot connect without it.
4. Press **Settings** on the Media Bridge card, then **Pair a phone**. Scan the QR code with the phone app, or choose the
   PC in the app's list. Both screens show the same six digits: approve on both.

Uninstalling removes the program and the manifest. The paired phones stay in `%APPDATA%\hesewake-media-bridge`; delete
that folder to forget them all.

## Privacy and security

- It listens on the local network only, on one TCP port, with TLS. The certificate is made on first run and kept on this PC
  (protected with DPAPI). The phone pins its SHA-256 fingerprint when it pairs.
- A phone can pair only while you pressed **Pair a phone** (two minutes), and only when the six digits match on both
  screens and you approve. The phone then gets a random token; only a hash of it is kept on the PC. **Forget** (here or on
  the phone) kills the token.
- Nothing leaves your network. There is no account, no cloud and no telemetry.
- `%APPDATA%\hesewake-media-bridge\bridge.log` records times, phone names and why a connection ended. It never holds song
  titles, artists or pictures. It is cut when it gets big.
- The notch runs the bridge in a Windows Job Object that does not let it start other programs.

## Build

```
cd bridge
cargo test --lib        # needs Rust (stable) and Windows
cargo build --release   # target\release\hesewake-media-bridge.exe (and the test tool fake_phone.exe)
```

`fake_phone` pretends to be a phone, to try the bridge without one: `cargo run --release --bin fake_phone`.
Two tests (`--ignored`) touch the real Windows media session and the network; run them by hand.

The installer is built with [Inno Setup 6](https://jrsoftware.org/isinfo.php): `powershell -File scripts/build-installer.ps1`
(it builds the program first). Pushing a tag `vX.Y.Z` that matches `bridge/Cargo.toml` makes GitHub Actions build the
installer and a draft release with `SHA256SUMS`.

`smtc-gate/` is the small experiment that showed a separate process can publish a media session; it is not shipped.

## Compatibility

| PC bridge | Phone app | What works |
|---|---|---|
| 0.1.x | 0.1.x | everything |
| 0.1.x | an older app without `capabilities` | media and controls; the picture is sent every second, phones are called "Phone N" |

The two sides agree on optional features when they connect (`capabilities`), so a newer side keeps working with an older one.

## License

MIT, see [LICENSE](LICENSE).
