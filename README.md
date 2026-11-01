# Hesewake Media Bridge (extension for Hesewake Notch)

Shows the media that plays on an Android phone in the notch, over the local network. The audio stays on the phone. The
extension publishes a Windows media session (SMTC), so the notch's Music slide, its lyrics and the Windows media controls
all work with it, and the buttons send play / pause / next / previous / seek back to the phone.

It is an extension (a separate program that the notch starts), not a part of the notch. See `docs/extensions.md` in the
notch repository for how extensions work, and `MEDIA_BRIDGE_ANDROID_NOTCH_PLAN.md` there for the wire protocol with the
phone app (`../hesewake-media-bridge-android`).

## Status

Nothing but the gate exists so far.

### Gate: can a separate process publish a media session? (1 November 2026)

`smtc-gate/` publishes a made-up session (title "Hesewake Gate Test Song", state *Paused* so it does not take the media keys
from the user's player) from a program with a hidden window, then starts itself again as a second process that reads the
sessions through GSMTC, the same API the notch uses.

Result, on Windows 11 (build 22000):

- the second process **finds the session** with its title, artist, position (42 s) and duration (200 s);
- the controls are what was published (play, next and previous enabled; pause is off while the state is Paused);
- the reader asks it to play (`TryPlayAsync` returns true) and the publisher **receives the Play button press**;
- the session is listed under the name of the exe (`smtc-gate.exe`); a nicer name needs an app identity (an AppUserModelID
  with a Start menu shortcut), which can come later;
- other sessions on the PC were listed and not touched.

So the architecture holds: the bridge can be an extension. Run it again with `cargo run --release` in `smtc-gate/`.

## What is left

1. Extension skeleton: pipe client (overlapped I/O, like the notch's host), hello, `status`, toasts for connect/disconnect.
2. The phone side: TLS server with a pinned certificate, DNS-SD advertisement, pairing with the six-digit code, token, forget.
3. A small settings window (opened by the notch's *Settings* button): paired devices, pairing code and approval, QR.
4. SMTC from the phone's snapshots (position projected from the receive time) and commands back.
5. Tests with the Python simulator of the Android project, then a real phone.
