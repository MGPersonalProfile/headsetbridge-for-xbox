# HeadsetBridge for Xbox

Hear your Xbox in **any headset plugged into your PC** — the
picture stays on the TV, you keep playing with the console's own controller,
and the game sound comes out of your PC headphones.

Xbox wireless headsets need Microsoft-certified hardware. This tool sidesteps
that: it opens the same **Remote Play** session the official app uses, but asks
the console for **audio only** and plays it on the PC. No video, no browser, no
Electron — a single 2.4 MB Windows executable using ~2.5 % of one CPU core.

> Not affiliated with, endorsed or sponsored by Microsoft. Xbox is a trademark
> of the Microsoft group of companies. Use at your own risk.

## What it does

| Mode | Use it for | How it behaves |
|---|---|---|
| **Play** (default) | Gaming | The console's audio in your PC headset with a 20 ms buffer. A tiny idle input keeps the session alive. |
| **Music** (`--sin-entrada --relevo-provisioned`) | Spotify or another music app on the console | Opens the session *without* a controller, so the console does not switch to "playing remotely" and keeps your music playing. The console drops controller-less sessions after ~60 s, so the tool hands over to a fresh session every ~57 s and hides the gap with a 3 s buffer and pitch-preserving time-stretching. |

One double-click starts it, another one stops it (single instance, no window).

## Requirements

- Windows 10/11, x64.
- An Xbox One or Xbox Series X|S with **remote play enabled**
  (Settings › Devices & connections › Remote features), on the same local
  network as the PC.
- **Sign-in:** the first time, the tool opens microsoft.com/link and shows a
  code to type there; sign in with your Xbox account and it starts playing.
  The session is kept in `%LOCALAPPDATA%\XboxSoloAudio\cuenta.json`. If you
  already use [Greenlight](https://github.com/unknownskl/greenlight), its
  session is imported once.

## Install

1. Download the `headsetbridge-windows` artifact from the latest
   [build](../../actions) and unzip it.
2. Run `powershell -ExecutionPolicy Bypass -File tools\install.ps1 -Exe rust\target\release\xbox-solo-audio.exe`.
   It copies the exe to `%LOCALAPPDATA%\Programs\XboxSoloAudioRust` and creates a
   desktop folder with a shortcut per mode (`-Volumen 100` for no extra gain;
   the default is 120 % with a soft limiter).

The exe is not code-signed, so Windows SmartScreen may warn the first time.
The shortcuts, the read-me that comes with them and the log are in Spanish for now.

## Known limits

- In Music mode, each hand-over (about once a minute) skips ~1.3 s of the song —
  the console does that — and the TV shows a short "started/stopped playing
  remotely" banner unless notification banners are turned off on the console
  (Settings › Preferences › Notifications).
- Audio quality is whatever the console sends: Opus, 48 kHz stereo, full band,
  36–130 kbps.
- Tested mainly on one Xbox Series S over Wi-Fi.

## How it works

- Signs in with Microsoft / Xbox Live and starts a home-streaming session on
  the xHome API.
- Talks the console's native **Nano V3** protocol directly (no WebRTC): ICE,
  SRTP AES-128-GCM with the key the session hands out, and the multiplexed
  channels (Control, Audio, Input…), opening only what audio needs.
- Decodes Opus with libopus and plays through WASAPI shared mode.

## Building

```powershell
# libopus 1.5.2 built in Release; see .github/actions/libopus
cargo build --release --manifest-path rust/Cargo.toml --package xbox-solo-audio
```

Optional features: `webrtc` (the WebRTC transport), `rustls`, `winhttp`.

## License

MIT. Parts of the native transport are ported from
[greenlight-native](https://github.com/unknownskl/greenlight-native) (MIT); see
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
