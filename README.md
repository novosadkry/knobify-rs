# Knobify

Control your Spotify volume with a hardware knob (or any key) from the
Windows tray. Turn the knob, see a Windows-style volume popup, and the volume
changes on whatever Spotify device is playing, wherever it is.

- **Global hotkeys** for volume up, volume down and mute, rebindable from the
  settings window by pressing the key you want.
- **On-screen popup** like the Windows volume flyout: transparent, click-through,
  always on top, with configurable duration and position.
- **Spotify login** with PKCE (client ID only, no secret), token cached per user.
- **Optional key swallowing** so a bound key never reaches Windows or other apps.

Windows only. Requires a **Spotify Premium** account (the Web API only allows
playback control for Premium users).

## Setup

1. Create a Spotify app at <https://developer.spotify.com/dashboard>.
   - Redirect URI: `http://127.0.0.1:8888/callback` (exactly; `localhost` is
     rejected by Spotify). If port 8888 is taken on your machine, change the
     port in Knobify's settings and register the matching URI.
   - Copy the **Client ID**. No client secret is needed.
2. Start `knobify.exe`. It appears in the tray (no window opens).
3. Right-click the tray icon → **Settings…**, paste the Client ID, click
   **Log in**. A browser tab opens; approve access; the tab tells you when you
   can close it.
4. In Settings → **Knob bindings**, click **Rebind** next to *Volume up*, turn
   the knob up (or press the key) and repeat for *Volume down* and, if you
   want, *Mute*. Keys Windows does not name show up as `Key 0x82`-style names.
5. Save. Turn the knob: the popup appears and Spotify's volume follows.

## Settings

Stored in `%APPDATA%\knobify\config.toml`; the login token in
`%APPDATA%\knobify\token.json` (delete it or use **Log out** to forget the login).

| Setting | Meaning |
|---|---|
| Client ID / Redirect port | Your Spotify app credentials and loopback port |
| Step per tick | Volume change per knob tick (1–25 %) |
| Swallow bound keys | Bound keys are consumed and never reach other apps (useful when a media key is bound) |
| Popup: enabled, duration, position, margin | How and where the volume popup appears |
| Popup: transparent | Per-pixel transparent popup window. Turn **off** (and restart) if your GPU driver renders it as a black rectangle |

Everything except *transparent* applies immediately on Save.

## Troubleshooting

| Popup says | Cause and fix |
|---|---|
| Not logged in | Log in from the tray menu or Settings |
| No active Spotify device | Start playback on any device first; Spotify only reports the active one |
| Volume control not allowed | That device rejects remote volume changes (some speakers, group sessions) |
| Spotify Premium required | Playback control is a Premium-only API |
| Slow down | Spotify rate limit; Knobify waits the requested time and retries |
| Offline | No network or Spotify unreachable |

Logs: run `knobify.exe` from a terminal with `RUST_LOG=debug` to see what the
hook and the Spotify service are doing.

## Building

Knobify is a cargo workspace: `knobify-core` (settings, volume model, Spotify
service; builds anywhere) and `knobify` (the Windows binary).

```powershell
# On Windows
cargo build --release --target x86_64-pc-windows-msvc
# exe: target\x86_64-pc-windows-msvc\release\knobify.exe
```

```sh
# Cross-compiling from Linux (default target is x86_64-pc-windows-gnu)
rustup target add x86_64-pc-windows-gnu
sudo apt-get install mingw-w64
cargo build --release
# exe: target/x86_64-pc-windows-gnu/release/knobify.exe

# Host-side tests of the core crate
cargo test -p knobify-core --target x86_64-unknown-linux-gnu
```

Cargo features of the `knobify` crate:

- `listen-only`: use a passive keyboard hook instead of the swallowing one
  (disables *Swallow bound keys*). Use if the default hook misbehaves.

## Manual smoke checklist

Things that can only be checked on a real Windows machine:

1. Tray icon appears; double-click opens Settings.
2. Settings → paste Client ID → Log in → browser → "logged in" status.
3. Turn the knob: popup appears at the configured position, Spotify volume changes, popup hides after the configured duration.
4. Rebind → press knob → the row shows the new key; Save; the new key works.
5. Enable *Swallow bound keys* with a media key bound: Windows' own volume no longer changes.
6. Toggle *transparent* off, restart: popup renders as an opaque dark panel.
7. Exit from the tray menu quits the process.

See `docs/ARCHITECTURE.md` for the design.
