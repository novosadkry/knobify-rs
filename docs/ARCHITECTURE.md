# Knobify architecture

Knobify is a Windows background utility that turns a keyboard knob (OEM
virtual-key codes such as 0x81 / 0x82) into Spotify volume control through the
Spotify Web API, with an on-screen volume popup (OSD), a settings window and a
tray icon.

## Crates

| Crate | Builds on | Contents |
|---|---|---|
| `knobify-core` | any host | settings (`config.rs`, `keycode.rs`), `VolumeModel`, `Coalescer`, `AppEvent` vocabulary, the Spotify service (PKCE auth, actor, error mapping) |
| `knobify` (bin) | Windows only | eframe app (`app.rs`), global key hook (`hotkeys.rs`), OSD (`ui/osd.rs`), settings viewport (`ui/settings.rs`), tray (`ui/tray.rs`), Win32 helpers (`win.rs`) |

`.cargo/config.toml` makes `x86_64-pc-windows-gnu` the default target (the bin
crate cannot build on Linux: tray-icon needs GTK, rdev needs X11/xdo). Test
the core on the host with `cargo test -p knobify-core --target x86_64-unknown-linux-gnu`.

## Threads and channels

```
rdev grab thread ─┐                                 ┌─> SpotifyHandle (tokio mpsc) ─> tokio thread: actor (all HTTP, Coalescer)
tray callbacks  ──┼─> AppHandle { mpsc<AppEvent>, egui::Context } ─> UI thread: App::logic drains, App::ui paints the OSD root
tokio actor     ──┘      send() = tx.send + ctx.request_repaint()   └─> Settings = deferred child viewport (Arc<Mutex<SettingsState>>)
```

* **UI thread** (eframe/winit main loop). Owns `KnobifyApp`, the tray icon and
  all viewports. Never blocks on I/O.
* **Hook thread** runs `rdev::grab` forever (rdev has no stop API). The
  callback executes inside a Windows low-level hook and must return in
  microseconds: `try_read` the bindings snapshot, match, `AppHandle::send`.
  Returning `None` swallows the key (only for bound keys when
  `bindings.suppress` is on, and for the captured key in capture mode).
* **Spotify thread** runs a current-thread tokio runtime hosting the actor,
  which owns the `AuthCodePkceSpotify` client, coalesces volume changes and
  reconciles with `current_playback`.

## Windowing model (eframe 0.36)

* The **root viewport is the OSD**: frameless, transparent, always-on-top,
  mouse-passthrough, `with_active(false)`, `with_taskbar(false)`, never hidden.
  `clear_color` is fully transparent; `App::ui` paints the rounded panel only
  while `OsdState::hide_at > now`, otherwise nothing.
  Reason: eframe runs only `update_logic_only` for a hidden root and processes
  only viewport *commands* there, never `viewport_output`, so a hidden root
  can neither create nor show a child viewport, and hidden-window repaints are
  throttled to a 100 ms heartbeat.
* After the first frame, `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` are added to
  the root HWND (winit's `with_taskbar(false)` only removes the taskbar button,
  not the Alt+Tab entry).
* **Settings is a deferred child viewport** (`ctx.show_viewport_deferred`)
  shown every root pass while open. Its callback edits `Arc<Mutex<SettingsState>>`
  and pushes `SettingsAction`s that `App::logic` drains.
* Renderer is **glow**. egui-wgpu selects `CompositeAlphaMode` from the
  adapter, and DX12 generally offers only `Opaque`, so wgpu cannot produce a
  transparent window on Windows. glow/WGL alpha is driver-dependent, therefore
  `osd.transparent = false` switches to an opaque dark flyout (restart needed).

## Spotify

* PKCE (`AuthCodePkceSpotify`), client ID only, scopes
  `user-modify-playback-state` and `user-read-playback-state`.
* Redirect URI `http://127.0.0.1:{port}/callback` (default port 8888). Spotify
  rejects `localhost`; the URI must be registered verbatim in the developer
  dashboard. The listener binds *before* the browser opens.
* Token cached at `<config dir>/knobify/token.json` (`token_cached`,
  `token_refreshing`). `read_token_cache` returns `None` when scopes differ.
* `get_authorize_url` stores the PKCE verifier on the client instance, so the
  same instance must call `request_token`.
* Error mapping (`spotify/errors.rs`): 401 → refresh once, then `NotLoggedIn`;
  403 "Restriction violated" → `VolumeControlNotAllowed`, 403 premium →
  `PremiumRequired`; 404 → `NoActiveDevice`; 429 → `RateLimited{retry_after}`;
  transport → `Offline`.
* Volume path: the UI updates `VolumeModel` instantly and shows the OSD; every
  change sends `SpotifyCmd::SetVolume`. The actor's `Coalescer` PUTs once after
  120 ms of quiet, re-arms if the target moved during a request, and backs off
  on 429. Reconcile with `current_playback` on login, 1.5 s after a burst, and
  when a burst starts after more than 30 s idle.

## Configuration

`<config dir>/knobify/config.toml` (Windows: `%APPDATA%\knobify\config.toml`),
written atomically. All structs have serde defaults.

```toml
version = 1
client_id = ""
redirect_port = 8888
step = 5                    # 1..=25

[bindings]
volume_up = "0x82"          # "Name" of an rdev::Key variant, or hex virtual-key code
volume_down = "0x81"
# mute = "VolumeMute"
suppress = false            # swallow bound keys so Windows never sees them

[osd]
enabled = true
duration_ms = 1500
position = "BottomCenter"   # TopLeft TopCenter TopRight BottomLeft BottomCenter BottomRight
margin = 48.0
transparent = true
```

## Coding rules

* No `unwrap()`/`expect()` on I/O, config, network or lock results in
  non-test code; surface failures as `UserFacing`/log messages.
* Never hold a `std::sync` lock across an `.await`.
* Nothing blocking on the UI thread; nothing slow in the hook callback.
* Windows-specific code lives in `win.rs`, `hotkeys.rs`, `ui/tray.rs`.
* docs.rs is not reachable from the development sandbox; read crate sources
  under `~/.cargo/registry/src/*/` instead.
