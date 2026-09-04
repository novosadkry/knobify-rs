//! Coalesces a burst of volume changes into as few Spotify API calls as
//! possible. Pure state machine driven by `Instant`s so it is unit-testable.
//!
//! Contract (implemented by the Spotify work package):
//! * [`Coalescer::on_local`] records the newest desired volume and arms a
//!   deadline `quiet` after `now`.
//! * [`Coalescer::take_send`] returns the volume to PUT when the deadline has
//!   passed, nothing is in flight, and the desired value differs from the last
//!   value confirmed by Spotify. It marks the request as in flight.
//! * [`Coalescer::on_sent_ok`] clears the in-flight flag; if the desired volume
//!   moved meanwhile, the deadline is re-armed immediately so the final value
//!   lands with at most one extra request.
//! * [`Coalescer::on_sent_err`] clears the in-flight flag and re-arms the
//!   deadline after `backoff` (for example a 429 `Retry-After`).
//! * [`Coalescer::next_deadline`] tells the actor when to wake up.

use std::time::{Duration, Instant};

pub const DEFAULT_QUIET: Duration = Duration::from_millis(120);

#[derive(Debug, Clone)]
pub struct Coalescer {
    quiet: Duration,
    desired: Option<u8>,
    last_sent: Option<u8>,
    deadline: Option<Instant>,
    in_flight: bool,
}

impl Default for Coalescer {
    fn default() -> Self {
        Self::new(DEFAULT_QUIET)
    }
}

impl Coalescer {
    pub fn new(quiet: Duration) -> Self {
        Self {
            quiet,
            desired: None,
            last_sent: None,
            deadline: None,
            in_flight: false,
        }
    }

    pub fn on_local(&mut self, volume: u8, now: Instant) {
        let _ = (volume, now, self.quiet);
        todo!("WP1: record desired volume and arm deadline")
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn take_send(&mut self, now: Instant) -> Option<u8> {
        let _ = (now, self.desired, self.last_sent, self.in_flight);
        todo!("WP1: return the volume to send when due")
    }

    pub fn on_sent_ok(&mut self, volume: u8, now: Instant) {
        let _ = (volume, now);
        todo!("WP1: confirm send, re-arm if desired moved")
    }

    pub fn on_sent_err(&mut self, backoff: Duration, now: Instant) {
        let _ = (backoff, now);
        todo!("WP1: re-arm after backoff")
    }

    /// Adopt a volume observed on the device (from `current_playback`).
    pub fn on_remote_observed(&mut self, volume: u8) {
        let _ = volume;
        todo!("WP1: update last_sent baseline")
    }
}
