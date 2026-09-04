# Knobify architecture

Knobify is a Windows background utility that turns a keyboard knob (OEM
function keys above F12, such as F13 / F15) into Spotify volume control through the
Spotify Web API, with an on-screen volume popup (OSD), a settings window and a
tray icon.

## Crates

| Crate | Builds on | Contents |
|---|---|---|
| `knobify-core` | any host | settings (`config.rs`, `keycode.rs`), `VolumeModel`, `Coalescer`, `AppEvent` vocabulary, the Spotify service (PKCE auth, actor, error mapping) |
| `knobify` (bin) | Windows only | eframe app (`app.rs`), global key hook (`hotkeys.rs`), OSD (`ui/osd.rs`), settings viewport (`ui/settings.rs`), tray (`ui/tray.rs`), Win32 helpers (`win.rs`) |

The bin crate is Windows-only (tray-icon needs GTK on Linux, and the keyboard
hook is Win32), so `cargo build` uses the host target on Windows and needs
`--target x86_64-pc-windows-gnu` on Linux; see `.cargo/config.toml`. Test the
core anywhere with `cargo test -p knobify-core`.

## Threads and channels

```
hook thread     ─┐                                  ┌─> SpotifyHandle (tokio mpsc) ─> tokio thread: actor (all HTTP, Coalescer)
tray callbacks  ──┼─> AppHandle { mpsc<AppEvent>, egui::Context } ─> UI thread: App::logic drains, App::ui paints the OSD root
tokio actor     ──┘      send() = tx.send + repaint_of(ROOT)        └─> Settings = deferred child viewport (Arc<Mutex<SettingsState>>)
```

* **UI thread** (eframe/winit main loop). Owns `KnobifyApp`, the tray icon and
  all viewports. Never blocks on I/O.
* **Hook thread** installs a `WH_KEYBOARD_LL` hook and pumps messages forever
  (the hook is dispatched only while its thread retrieves messages, and dies
  with that thread). The callback must return in microseconds: `try_read` the
  bindings, match on the virtual-key code, queue through `HookSender`.
  Returning `1` swallows the key - only for a bound key while
  `bindings.suppress` is on, and for the key a rebind is waiting for.
  Deliberately **not** a hook library: see the module docs of `hotkeys.rs`.
* **Hook waker thread** exists only so the callback never has to call egui.
  `HookSender::send` queues the event and unparks this thread, which then does
  the `request_repaint_of(ROOT)` that wakes the UI. Anything that can block on
  a lock the UI thread holds - egui calls, logging - must stay out of the
  callback, or Windows destroys the hook the first time it overruns 300 ms.
* The hook is **re-installed every 5 seconds** from a thread timer in that same
  message loop. Two syscalls, and it turns "the hook died for reasons we did
  not foresee" from a dead session into at most five seconds of dead keys.
* **Rebinding does not use the hook at all.** While a rebind is armed,
  `App::logic` polls `win::poll_pressed_key` (`GetAsyncKeyState`, whose low bit
  is "pressed since last asked") and takes the first non-modifier key. A rebind
  that silently does nothing leaves the user unable to fix a wrong binding, so
  it must not share a failure mode with the thing it exists to configure.
* **Spotify thread** runs a current-thread tokio runtime hosting the actor,
  which owns the `AuthCodePkceSpotify` client, coalesces volume changes and
  reconciles with `current_playback`.

## Windowing model (eframe 0.36)

* **Mouse passthrough is not `with_mouse_passthrough`.** winit implements that
  as `WS_EX_TRANSPARENT | WS_EX_LAYERED`, and a layered window over an OpenGL
  surface composites unreliably: on real hardware the popup stayed invisible
  while painting at full opacity, and moving it off-screen and back never
  brought it back. `apply_osd_exstyles` sets `WS_EX_TRANSPARENT` itself (all
  click-through needs) and clears `WS_EX_LAYERED`.
* Idle means the root window is **hidden** (`ViewportCommand::Visible`), not
  mapped-and-empty and not parked off-screen: an empty pass is a grey rectangle
  wherever the driver ignores alpha. It stays mapped, parked off-screen, only
  while Settings is open, since a hidden root cannot host a child viewport.
* The **root viewport is the OSD**: frameless, transparent, always-on-top,
  click-through, `with_active(false)`, `with_taskbar(false)`.
  `clear_color` is fully transparent; `App::ui` paints the rounded panel only
  while `OsdState::hide_at > now`, otherwise nothing.
  Reason: eframe runs only `update_logic_only` for a hidden root and processes
  only viewport *commands* there, never `viewport_output`, so a hidden root
  can neither create nor show a child viewport, and hidden-window repaints are
  throttled to a 100 ms heartbeat.
* `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` are added to the root HWND (winit's
  `with_taskbar(false)` only removes the taskbar button, not the Alt+Tab entry).
  They are re-applied on the **first two** passes and after every visibility
  toggle: winit's `WindowFlags::apply_diff` rewrites the whole `GWL_EXSTYLE`
  word on any flag change, and eframe calls `set_visible(true)` right after the
  first painted frame, which would otherwise drop the bits again. `App::ui`
  requests one extra repaint so that second pass exists at all (an idle popup
  asks for no repaints).
* **Settings is a deferred child viewport** (`ctx.show_viewport_deferred`)
  shown every root pass while open. Its callback edits `Arc<Mutex<SettingsState>>`
  and pushes `SettingsAction`s that `App::logic` drains. A repaint of a deferred
  viewport runs **only its callback**, not `App::logic`, so the callback ends
  with `request_repaint_of(ViewportId::ROOT)` whenever it queued an action -
  without it every button in the window would look dead until something else
  woke the root. The same applies in reverse: the root repaints the settings
  viewport (`request_repaint_of(settings::viewport_id())`) after it changes
  `SettingsState`. `AppHandle::send` also targets `ViewportId::ROOT` explicitly.
* Closing the window (X or **Close**) queues `SettingsAction::Close`; the root
  drops `settings_ui`, stops showing the viewport, and egui destroys the window.
  Reopening from the tray builds a fresh `SettingsState`.
* In the **opaque fallback** the root has to stay mapped while Settings is open
  (a hidden root cannot host a child viewport), but an empty opaque pass is a
  dark rectangle, so `OsdState` parks the window off-screen until the popup has
  something to paint.
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
* `Playback` snapshots move `VolumeModel` **only** when the last knob tick is
  more than 1.5 s old (`BURST_GRACE` in `app.rs`), so a reconcile never yanks
  the bar while the user is still turning. `VolumeApplied` clears the pending
  marker (the dot on the popup) once the applied value equals the local one.
* `PlaybackSnapshot::supports_volume == false` disables the volume path: the
  next tick shows "Volume control not allowed" instead of a bar that cannot
  move, and re-reads the playback state at most every 5 s so switching to a
  device that does allow it recovers by itself.
* The actor refreshes `current_playback` itself after a restore or a login; the
  UI must not send its own `Refresh` on `Auth(LoggedIn)`.
* A panic in the actor thread is reported as
  `Error(Other("Spotify service crashed, restart Knobify"))` by a `Drop` guard
  on that thread, instead of leaving volume control silently dead.

## Configuration

`<config dir>/knobify/config.toml` (Windows: `%APPDATA%\knobify\config.toml`),
written atomically. All structs have serde defaults.

```toml
version = 1
client_id = ""
redirect_port = 8888
step = 5                    # 1..=25

[bindings]
volume_up = "0x7C"          # hex virtual-key code, or a name like "F13"/"VolumeUp"
volume_down = "0x7E"        # (0x7C/0x7E are F13/F15, which is what one knob sends)
# mute = "VolumeMute"
suppress = false            # swallow bound keys so Windows never sees them

[osd]
enabled = true
duration_ms = 1500
position = "BottomCenter"   # TopLeft TopCenter TopRight BottomLeft BottomCenter BottomRight
margin = 48.0
transparent = true
```

## Logging

`env_logger` writes to `<config dir>/knobify/knobify.log`, truncated on every
start (`info` by default, `RUST_LOG` still honoured); debug builds also echo to
stderr. The release binary is linked with `windows_subsystem = "windows"`, so it
has no console and stderr goes nowhere even when started from a terminal - hence
the file. One info line each per start records the config path, the log path,
whether the token cache exists, the loaded settings summary, the keyboard hook
and the first popup placement (work area, source, DPI, result); later placements
and each bound key press are debug. Note that rspotify's HTTP target is muted to
`warn` by default because it logs request headers - including the bearer token -
at info level, so `RUST_LOG=debug` re-enables that; prefer
`RUST_LOG=knobify=debug`.

## Verified crate behaviour

Things that are easy to get wrong and were read out of the vendored sources
(docs.rs is unreachable from the sandbox; sources are under
`~/.cargo/registry/src/*/`):

* **rspotify-model 0.16.1 `Device` has no `supports_volume` field** (only `id`,
  `is_active`, `is_private_session`, `is_restricted`, `name`, `type`,
  `volume_percent`), so `PlaybackSnapshot::supports_volume` is derived as
  `volume_percent.is_some() && !is_restricted`.
* **`read_token_cache` errors when the cache file is missing** - `Token::from_cache`
  starts with `File::open(..)?` - which is the normal first-run state, so
  `restore_session` maps any error to "not logged in" and logs at debug. It
  returns `Ok(None)` for a token whose scopes are a subset mismatch.
* **rdev 0.5.3 cannot be used for the hook.** Its `WH_KEYBOARD_LL` callback
  resolves a printable name for every key press before handing the event on,
  which calls `AttachThreadInput` against the foreground window's thread and
  `ToUnicodeEx`. That can block longer than `LowLevelHooksTimeout` (300 ms), and
  Windows silently removes a hook that overruns it - so the first real keystroke
  killed the hook, with no error anywhere. Injected keys carry no scan code and
  skip the slow path, so synthetic tests passed while a keyboard did not. The
  hook is now installed directly (`hotkeys.rs`), and identifies keys by
  virtual-key code without any name lookup.
* **Function keys above F12 are ordinary virtual-key codes**: F13 is `0x7C` and
  they run to F24 at `0x87`, which is what knobs and macro pads commonly send.
  `KeyCode` therefore names any code it can (`0x7C` displays as `F13`) and
  matches names against codes, so `"F13"` and `"0x7C"` are one binding.
* **winit 0.30 rewrites both style words** (`SetWindowLongW(GWL_STYLE/GWL_EXSTYLE)`)
  in `WindowFlags::apply_diff` whenever any window flag changes, and it never
  sets `WS_EX_TOOLWINDOW`/`WS_EX_NOACTIVATE` itself - see the windowing notes
  above for how Knobify keeps them.
* **eframe 0.36 runs a deferred viewport's callback alone**, without
  `App::logic`, and starts the root window hidden until the first frame is
  painted (`Integration::post_rendering` -> `set_visible(true)`).

## Coding rules

* No `unwrap()`/`expect()` on I/O, config, network or lock results in
  non-test code; surface failures as `UserFacing`/log messages.
* Never hold a `std::sync` lock across an `.await`.
* Nothing blocking on the UI thread; nothing slow in the hook callback.
* Windows-specific code lives in `win.rs`, `hotkeys.rs`, `ui/tray.rs`.
* docs.rs is not reachable from the development sandbox; read crate sources
  under `~/.cargo/registry/src/*/` instead.
