//! Thread-safe handle for pushing events into the UI thread.

use std::sync::mpsc;

use knobify_core::spotify::SpotifyEvent;
use knobify_core::AppEvent;

/// Cloneable, `Send + Sync`. `send` queues the event and wakes the egui loop.
#[derive(Clone)]
pub struct AppHandle {
    tx: mpsc::Sender<AppEvent>,
    ctx: egui::Context,
}

impl AppHandle {
    pub fn new(ctx: egui::Context) -> (Self, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = mpsc::channel();
        (Self { tx, ctx }, rx)
    }

    pub fn send(&self, event: AppEvent) {
        if self.tx.send(event).is_err() {
            log::debug!("UI receiver dropped; event discarded");
            return;
        }
        // Only the root viewport runs `App::logic`, which is what drains this
        // channel, so wake that one explicitly instead of relying on
        // `request_repaint`'s "current viewport" (unset off the UI thread).
        self.ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    pub fn send_spotify(&self, event: SpotifyEvent) {
        self.send(AppEvent::Spotify(event));
    }

    /// The queue on its own, without the ability to wake the UI.
    ///
    /// For callers that must not touch egui - see [`HookSender`].
    pub fn sender(&self) -> mpsc::Sender<AppEvent> {
        self.tx.clone()
    }

    /// Ask the root viewport for a repaint, which is what drains the queue.
    pub fn repaint_root(&self) {
        self.ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// Queues events from inside a Windows low-level keyboard hook.
///
/// A hook callback must return in well under `LowLevelHooksTimeout` (300 ms by
/// default) or Windows silently destroys the hook, so it may not do anything
/// that can block on a lock the UI thread holds: no logging (the log writer is
/// shared and writes to a file) and above all no egui calls, since
/// `request_repaint_of` takes egui's context lock and the UI thread holds that
/// while it renders. Both were exactly how this app kept losing its hook.
///
/// So the callback only queues, then unparks a thread whose one job is to do
/// the egui wake-up where blocking is harmless. `Thread::unpark` is lock-free.
#[derive(Clone)]
pub struct HookSender {
    tx: mpsc::Sender<AppEvent>,
    waker: std::thread::Thread,
}

impl HookSender {
    /// Spawns the waker thread that this sender pokes.
    pub fn new(handle: &AppHandle) -> Self {
        let waker_handle = handle.clone();
        let waker = std::thread::Builder::new()
            .name("knobify-hook-waker".into())
            .spawn(move || loop {
                std::thread::park();
                waker_handle.repaint_root();
            })
            .map(|joined| joined.thread().clone())
            // Without the waker the UI still drains the queue on its next pass
            // (a tick, a tray click); only the immediate repaint is lost.
            .unwrap_or_else(|e| {
                log::error!("cannot start the hook waker thread: {e}");
                std::thread::current()
            });
        Self {
            tx: handle.sender(),
            waker,
        }
    }

    /// Queue an event. Never blocks on anything the UI thread can hold.
    pub fn send(&self, event: AppEvent) {
        if self.tx.send(event).is_ok() {
            self.waker.unpark();
        }
    }
}
