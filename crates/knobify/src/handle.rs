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
        self.ctx.request_repaint();
    }

    pub fn send_spotify(&self, event: SpotifyEvent) {
        self.send(AppEvent::Spotify(event));
    }
}
