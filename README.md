# Knobify

Control your Spotify volume with a hardware knob (or any key) from the
Windows tray. Turn the knob, see a Windows-style volume popup, and the volume
changes on whatever Spotify device is playing, wherever it is.

- **Global hotkeys** for volume up, volume down and mute, rebindable from the
  settings window by pressing the key you want.
- **On-screen popup** like the Windows volume flyout: transparent, click-through,
  always on top, with configurable duration and position.
- **No jumping**: the knob is a relative control, so before the first tick of a
  turn Knobify reads the volume back from Spotify. Change it in the Spotify app
  or on your phone and the next turn continues from there, not from whatever
  Knobify last remembered.
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
   want, *Mute*. Knobs usually turn out to send a function key above F12, so
   expect names like `F13`; the defaults are F13 and F15.
5. Save. Turn the knob: the popup appears and Spotify's volume follows.

## Settings

Stored in `%APPDATA%\knobify\config.toml`; the login token in
`%APPDATA%\knobify\token.json` (delete it or use **Log out** to forget the login).

| Setting | Meaning |
|---|---|
| Client ID / Redirect port | Your Spotify app credentials and loopback port |
| Step per tick | Volume change per knob tick (1–25 %) |
| Swallow bound keys | Bound keys are consumed and never reach other apps (useful when a media key is bound) |
| Spotify sync delay | How long Spotify takes to report a volume you just set (default 3000 ms). Inside this window Knobify trusts its own value; after it, it re-reads Spotify before the next turn. Raise it if a turn of the knob gets pulled back to an older value |
| Popup: enabled, duration, position, margin | How and where the volume popup appears |
| Popup: transparent | Per-pixel transparent popup window. Turn **off** (and restart) if your GPU driver renders it as a black rectangle |

Everything except *transparent* applies immediately on Save.

## Troubleshooting

| Popup says | Cause and fix |
|---|---|
| Set up Spotify | No client ID yet: open Settings from the tray and paste it (the tray item reads *Set up Spotify…* until then) |
| Not logged in | Log in from the tray menu or Settings |
| No active Spotify device | Start playback on any device first; Spotify only reports the active one |
| Volume control not allowed | That device rejects remote volume changes (some speakers, group sessions). Knobify stops sending until you switch device; it re-checks every few seconds |
| Spotify Premium required | Playback control is a Premium-only API |
| Slow down | Spotify rate limit; Knobify waits the requested time and retries |
| Offline | No network or Spotify unreachable |

Logs: `%APPDATA%\knobify\knobify.log`, rewritten on every start. It records the
config path, whether a login token is cached, whether the keyboard hook was
installed and where the popup was placed. Set `RUST_LOG=knobify=debug` before
starting `knobify.exe` for more detail — the release build has no console, so
the file is the only output.

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

### Identifying a key

Click **Rebind** in Settings and press the key: the row shows what it sent, so
there is no need to know its code in advance. Many knobs and macro pads send
the function keys above F12, which is why a knob often shows up as `F13`
(`0x7C`) rather than something volume-shaped. If the row never changes, that
key sends no keyboard input at all and nothing built on a keyboard hook can
bind it.

## Manual smoke checklist

Things that can only be checked on a real Windows machine:

1. Tray icon appears; double-click opens Settings.
2. Settings → paste Client ID → Log in → browser → "logged in" status.
3. Turn the knob: popup appears at the configured position, Spotify volume changes, popup hides after the configured duration.
4. Change the volume in the Spotify app, then turn the knob: it continues from
   the value Spotify shows, not from the last value Knobify sent.
5. Turn the knob a long way in one go, pause a second, keep turning: the value
   never gets pulled back to an earlier one.
6. Rebind → press knob → the row shows the new key; Save; the new key works.
7. Enable *Swallow bound keys* with a media key bound: Windows' own volume no longer changes.
8. Toggle *transparent* off, restart: popup renders as an opaque dark panel.
9. Exit from the tray menu quits the process (check Task Manager: no leftover
   `knobify.exe`).
10. The popup window is not in Alt+Tab and never takes focus, and clicks go
    through it to whatever is underneath.
11. With *transparent* off and Settings open, no dark rectangle is left on the
    desktop while the popup is idle.

See `docs/ARCHITECTURE.md` for the design.
