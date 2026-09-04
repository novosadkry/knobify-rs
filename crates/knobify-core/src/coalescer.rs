//! Coalesces a burst of volume changes into as few Spotify API calls as
//! possible. Pure state machine driven by `Instant`s so it is unit-testable.
//!
//! Contract:
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
        self.desired = Some(volume);
        self.deadline = Some(now + self.quiet);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The volume the last local change asked for, if any.
    pub fn desired(&self) -> Option<u8> {
        self.desired
    }

    /// The last volume Spotify confirmed (or that was observed on the device).
    pub fn last_sent(&self) -> Option<u8> {
        self.last_sent
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// True while neither a request nor a deadline is outstanding, i.e. the
    /// next local change starts a fresh burst.
    pub fn is_idle(&self) -> bool {
        !self.in_flight && self.deadline.is_none()
    }

    pub fn take_send(&mut self, now: Instant) -> Option<u8> {
        if self.in_flight {
            return None;
        }
        match self.deadline {
            Some(deadline) if now >= deadline => {}
            _ => return None,
        }

        let desired = self.desired;
        // The deadline fired; whatever happens now, it is spent.
        self.deadline = None;
        let volume = desired?;
        if Some(volume) == self.last_sent {
            return None;
        }
        self.in_flight = true;
        Some(volume)
    }

    pub fn on_sent_ok(&mut self, volume: u8, now: Instant) {
        self.in_flight = false;
        self.last_sent = Some(volume);
        if self.desired.is_some_and(|desired| desired != volume) {
            // The knob moved while the request was in flight: send the final
            // value right away instead of waiting out another quiet period.
            self.deadline = Some(now);
        } else {
            self.deadline = None;
        }
    }

    pub fn on_sent_err(&mut self, backoff: Duration, now: Instant) {
        self.in_flight = false;
        self.deadline = Some(now + backoff);
    }

    /// Give up on the pending volume (repeated failures). Clears the in-flight
    /// flag and the deadline without touching the confirmed baseline, so the
    /// next local change starts over.
    pub fn abandon(&mut self) {
        self.in_flight = false;
        self.deadline = None;
        self.desired = self.last_sent;
    }

    /// Adopt a volume observed on the device (from `current_playback`).
    pub fn on_remote_observed(&mut self, volume: u8) {
        self.last_sent = Some(volume);
        if self.is_idle() {
            self.desired = Some(volume);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Duration = Duration::from_millis(120);

    fn coalescer() -> Coalescer {
        Coalescer::new(QUIET)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn burst_of_ten_ticks_sends_once() {
        let mut c = coalescer();
        let t0 = Instant::now();
        c.on_remote_observed(50);

        // Ten ticks spread over 50 ms, 5 ms apart.
        for i in 0..10u64 {
            let now = t0 + ms(i * 5);
            c.on_local(51 + i as u8, now);
            assert_eq!(c.take_send(now), None, "no send inside the burst");
        }

        // Still quiet-1 ms after the last tick: nothing yet.
        let last_tick = t0 + ms(45);
        assert_eq!(c.take_send(last_tick + QUIET - ms(1)), None);

        // Exactly one send, carrying the final value.
        let due = last_tick + QUIET;
        assert_eq!(c.take_send(due), Some(60));
        assert_eq!(c.take_send(due), None, "second call while in flight");
        assert!(c.in_flight());
        assert_eq!(c.next_deadline(), None);

        c.on_sent_ok(60, due + ms(30));
        assert!(!c.in_flight());
        assert_eq!(c.next_deadline(), None, "nothing left to send");
        assert_eq!(c.take_send(due + ms(1000)), None);
    }

    #[test]
    fn movement_during_flight_sends_exactly_one_follow_up() {
        let mut c = coalescer();
        let t0 = Instant::now();
        c.on_remote_observed(40);

        c.on_local(45, t0);
        assert_eq!(c.take_send(t0 + QUIET), Some(45));

        // Knob keeps moving while the PUT is in flight.
        for (i, v) in [46u8, 47, 48].into_iter().enumerate() {
            let now = t0 + QUIET + ms(10 * (i as u64 + 1));
            c.on_local(v, now);
            assert_eq!(c.take_send(now), None, "in flight, nothing may be sent");
        }

        let done = t0 + QUIET + ms(50);
        c.on_sent_ok(45, done);
        // Re-armed immediately, not after another quiet period.
        assert_eq!(c.next_deadline(), Some(done));
        assert_eq!(c.take_send(done), Some(48));

        c.on_sent_ok(48, done + ms(20));
        assert_eq!(c.next_deadline(), None);
        assert_eq!(c.take_send(done + ms(500)), None, "exactly one follow-up");
    }

    #[test]
    fn value_equal_to_last_sent_is_not_sent() {
        let mut c = coalescer();
        let t0 = Instant::now();
        c.on_remote_observed(70);

        c.on_local(70, t0);
        assert_eq!(c.take_send(t0 + QUIET), None);
        assert!(!c.in_flight());
        assert_eq!(c.next_deadline(), None, "deadline is consumed, not retried");

        // A round trip that ends where it started also stays silent.
        c.on_local(75, t0 + ms(200));
        c.on_local(70, t0 + ms(210));
        assert_eq!(c.take_send(t0 + ms(210) + QUIET), None);
    }

    #[test]
    fn error_backs_off_then_retries_the_same_value() {
        let mut c = coalescer();
        let t0 = Instant::now();
        c.on_remote_observed(30);

        c.on_local(35, t0);
        let first = t0 + QUIET;
        assert_eq!(c.take_send(first), Some(35));

        let backoff = Duration::from_secs(3);
        c.on_sent_err(backoff, first);
        assert!(!c.in_flight());
        assert_eq!(c.next_deadline(), Some(first + backoff));
        assert_eq!(c.take_send(first + ms(500)), None, "still backing off");

        // The value was never confirmed, so it is retried unchanged.
        assert_eq!(c.take_send(first + backoff), Some(35));
        assert_eq!(c.last_sent(), Some(30));

        c.on_sent_ok(35, first + backoff + ms(10));
        assert_eq!(c.last_sent(), Some(35));
    }

    #[test]
    fn abandon_drops_the_pending_value() {
        let mut c = coalescer();
        let t0 = Instant::now();
        c.on_remote_observed(20);
        c.on_local(25, t0);
        assert_eq!(c.take_send(t0 + QUIET), Some(25));

        c.abandon();
        assert!(!c.in_flight());
        assert_eq!(c.next_deadline(), None);
        assert_eq!(c.take_send(t0 + ms(10_000)), None);

        // A fresh local change still works afterwards.
        c.on_local(26, t0 + ms(10_000));
        assert_eq!(c.take_send(t0 + ms(10_000) + QUIET), Some(26));
    }

    #[test]
    fn remote_observation_only_overrides_desired_when_idle() {
        let mut c = coalescer();
        let t0 = Instant::now();

        c.on_local(80, t0);
        c.on_remote_observed(10);
        assert_eq!(c.desired(), Some(80), "a pending change wins");
        assert_eq!(c.last_sent(), Some(10));
        assert_eq!(c.take_send(t0 + QUIET), Some(80));

        c.on_sent_ok(80, t0 + QUIET);
        c.on_remote_observed(15);
        assert_eq!(c.desired(), Some(15), "idle: adopt the device value");
        assert_eq!(c.next_deadline(), None, "adopting must not schedule a PUT");
    }
}
