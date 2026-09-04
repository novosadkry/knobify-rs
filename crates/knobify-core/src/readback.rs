//! Reads the real volume back from Spotify before a knob tick works from it.
//!
//! The knob is a *relative* control: a tick means "five percent louder than it
//! is now". Knobify only knows "now" from the last value it sent or read, so a
//! volume changed anywhere else - the Spotify app, a phone, another device -
//! leaves that baseline wrong, and the next tick jumps the volume to a value
//! nobody asked for.
//!
//! Polling the device every few seconds would only shrink that window. Instead
//! a tick that arrives on a stale baseline is shown in the popup at once (the
//! popup must never feel laggy) while its request is held back and the current
//! volume is read; the held ticks are then replayed on top of the answer, so
//! exactly one request goes out and it carries the right value. Ticks that
//! arrive during the wait join the replay, which is what makes a fast turn
//! still cost one read and one write.
//!
//! Holding the request is what makes the reading trustworthy: because nothing
//! has been sent yet, the value Spotify reports cannot be an echo of this
//! app's own change, so there is no way to double-count a tick.
//!
//! The other half of the job is knowing when *not* to believe Spotify.
//! `GET /me/player` reports a volume change the API has already accepted only
//! some time later - measured against the real service, between 0.4 s and
//! 2.4 s - so a reading taken inside that window still carries the previous
//! volume, and adopting it would quietly undo the turn the user just made.
//! [`Readback::may_adopt`] is therefore the single gate every snapshot has to
//! pass, and the window it uses is the same one that decides staleness: after
//! a change of our own, the local value is the authority until Spotify has had
//! time to catch up; once it has, Spotify is.
//!
//! Pure state machine driven by `Instant`s, like [`crate::coalescer`].

use std::time::{Duration, Instant};

use crate::events::HotkeyAction;

/// How long Spotify may take to report a volume this app set.
///
/// A baseline older than this is re-read before the knob works from it, and
/// nothing Spotify reports inside it is believed. Overridden from settings
/// (`sync_delay_ms`); this is only the fallback for a default `Readback`.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_millis(3000);

/// How long a held tick waits for the reading before being sent anyway.
///
/// Long enough for a Spotify round trip, short enough that a turn made while
/// offline still lands within an unnoticed moment.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(600);

/// What to do with a knob tick that has already been applied locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickPlan {
    /// The baseline is fresh: send the new volume now.
    Send,
    /// The baseline is stale: ask Spotify for the current volume and hold the
    /// tick until it answers.
    ReadBack,
    /// A read-back is already in flight and will carry this tick too.
    Wait,
}

#[derive(Debug, Clone)]
struct Pending {
    /// Ticks applied since the read-back was asked for, in order.
    actions: Vec<HotkeyAction>,
    /// When the read-back was asked for (for the timeout).
    since: Instant,
}

#[derive(Debug, Clone)]
pub struct Readback {
    stale_after: Duration,
    timeout: Duration,
    /// Reading the volume back is pointless while logged out.
    logged_in: bool,
    /// When the volume was last known for certain: a device snapshot, or a
    /// value Spotify accepted.
    synced_at: Option<Instant>,
    /// When the knob last moved, so a snapshot cannot land mid-turn.
    ticked_at: Option<Instant>,
    pending: Option<Pending>,
}

impl Default for Readback {
    fn default() -> Self {
        Self::new(DEFAULT_STALE_AFTER, DEFAULT_TIMEOUT)
    }
}

impl Readback {
    pub fn new(stale_after: Duration, timeout: Duration) -> Self {
        Self {
            stale_after,
            timeout,
            logged_in: false,
            synced_at: None,
            ticked_at: None,
            pending: None,
        }
    }

    /// Follows the `sync_delay_ms` setting.
    pub fn set_stale_after(&mut self, stale_after: Duration) {
        self.stale_after = stale_after;
    }

    /// Follows the Spotify auth state. Logging out drops the baseline and any
    /// held tick: no reading is coming, and the value could not be sent anyway.
    pub fn set_logged_in(&mut self, logged_in: bool) {
        self.logged_in = logged_in;
        if !logged_in {
            self.synced_at = None;
            self.pending = None;
        }
    }

    /// Whether a device snapshot may move the volume model.
    ///
    /// Two things have to be true. Nothing this app sent can still be
    /// settling, because a snapshot taken while it is reports the volume from
    /// *before* that change and adopting it would undo the user's turn. And
    /// the knob must not have moved recently, or the bar would jump under the
    /// user's fingers. Both are the same window: until Spotify has had time to
    /// catch up, the local value is the authority.
    pub fn may_adopt(&self, now: Instant) -> bool {
        self.elapsed(self.synced_at, now) && self.elapsed(self.ticked_at, now)
    }

    fn elapsed(&self, at: Option<Instant>, now: Instant) -> bool {
        at.is_none_or(|at| now.saturating_duration_since(at) >= self.stale_after)
    }

    /// The volume is known as of `now` (a snapshot, or an accepted send).
    pub fn on_synced(&mut self, now: Instant) {
        self.synced_at = Some(now);
    }

    /// Plan for a tick that the caller has already applied to its own model.
    pub fn on_tick(&mut self, action: HotkeyAction, now: Instant) -> TickPlan {
        self.ticked_at = Some(now);
        if let Some(pending) = &mut self.pending {
            pending.actions.push(action);
            return TickPlan::Wait;
        }
        if !self.logged_in || !self.is_stale(now) {
            return TickPlan::Send;
        }
        self.pending = Some(Pending {
            actions: vec![action],
            since: now,
        });
        TickPlan::ReadBack
    }

    fn is_stale(&self, now: Instant) -> bool {
        self.elapsed(self.synced_at, now)
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The ticks to replay on a reading that has just arrived, if any were
    /// waiting for it.
    pub fn take_replay(&mut self) -> Option<Vec<HotkeyAction>> {
        self.pending.take().map(|pending| pending.actions)
    }

    /// When the held tick gives up waiting, for scheduling a wake-up.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending
            .as_ref()
            .map(|pending| pending.since + self.timeout)
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }

    /// Stop waiting for a reading. Returns true when a tick was being held, in
    /// which case the caller must send the volume it applied locally rather
    /// than swallow the turn.
    pub fn abandon(&mut self) -> bool {
        self.pending.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALE: Duration = Duration::from_secs(1);
    const TIMEOUT: Duration = Duration::from_millis(600);

    fn readback() -> Readback {
        let mut r = Readback::new(STALE, TIMEOUT);
        r.set_logged_in(true);
        r
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn first_tick_of_the_session_reads_back() {
        let mut r = readback();
        let t0 = Instant::now();
        // Nothing has ever been synced, so the baseline is not a baseline.
        assert_eq!(r.on_tick(HotkeyAction::VolumeUp, t0), TickPlan::ReadBack);
        assert!(r.is_pending());
    }

    #[test]
    fn a_fresh_baseline_sends_straight_away() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_synced(t0);

        assert_eq!(
            r.on_tick(HotkeyAction::VolumeUp, t0 + ms(200)),
            TickPlan::Send
        );
        assert!(!r.is_pending(), "nothing is held back");
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn a_baseline_older_than_stale_after_reads_back() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_synced(t0);

        assert_eq!(
            r.on_tick(HotkeyAction::VolumeDown, t0 + STALE - ms(1)),
            TickPlan::Send
        );
        assert_eq!(
            r.on_tick(HotkeyAction::VolumeDown, t0 + STALE),
            TickPlan::ReadBack
        );
    }

    #[test]
    fn ticks_during_the_wait_join_the_replay() {
        let mut r = readback();
        let t0 = Instant::now();

        assert_eq!(r.on_tick(HotkeyAction::VolumeUp, t0), TickPlan::ReadBack);
        for i in 1..4u64 {
            assert_eq!(
                r.on_tick(HotkeyAction::VolumeUp, t0 + ms(i * 10)),
                TickPlan::Wait,
                "one read-back per turn, not one per tick"
            );
        }

        assert_eq!(r.take_replay(), Some(vec![HotkeyAction::VolumeUp; 4]));
        assert!(!r.is_pending());
        assert_eq!(r.take_replay(), None, "the replay is taken once");
    }

    #[test]
    fn the_replay_keeps_the_order_of_the_ticks() {
        let mut r = readback();
        let t0 = Instant::now();

        r.on_tick(HotkeyAction::VolumeUp, t0);
        r.on_tick(HotkeyAction::MuteToggle, t0 + ms(5));
        r.on_tick(HotkeyAction::VolumeDown, t0 + ms(10));

        assert_eq!(
            r.take_replay(),
            Some(vec![
                HotkeyAction::VolumeUp,
                HotkeyAction::MuteToggle,
                HotkeyAction::VolumeDown,
            ])
        );
    }

    #[test]
    fn a_synced_reading_makes_the_next_tick_immediate() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_tick(HotkeyAction::VolumeUp, t0);

        // The reading arrives and is replayed.
        let arrived = t0 + ms(150);
        r.on_synced(arrived);
        assert!(r.take_replay().is_some());

        assert_eq!(
            r.on_tick(HotkeyAction::VolumeUp, arrived + ms(10)),
            TickPlan::Send,
            "the rest of the turn must not wait for anything"
        );
    }

    #[test]
    fn a_held_tick_expires_and_is_sent_anyway() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_tick(HotkeyAction::VolumeUp, t0);
        assert_eq!(r.deadline(), Some(t0 + TIMEOUT));

        assert!(!r.expired(t0 + TIMEOUT - ms(1)));
        assert!(r.expired(t0 + TIMEOUT));
        assert!(r.abandon(), "the caller has a turn to send");
        assert!(!r.is_pending());
        assert!(!r.abandon(), "and only hears about it once");
    }

    #[test]
    fn the_deadline_is_set_by_the_first_tick_of_the_turn() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_tick(HotkeyAction::VolumeUp, t0);
        r.on_tick(HotkeyAction::VolumeUp, t0 + ms(400));
        // Otherwise a slow, steady turn could put the send off indefinitely.
        assert_eq!(r.deadline(), Some(t0 + TIMEOUT));
    }

    #[test]
    fn a_snapshot_is_not_adopted_while_our_own_change_may_still_be_settling() {
        let mut r = readback();
        let t0 = Instant::now();
        // Spotify accepted a volume: for the next STALE it may still report
        // the one before it, so nothing it says can be believed.
        r.on_synced(t0);

        assert!(!r.may_adopt(t0 + ms(500)));
        assert!(!r.may_adopt(t0 + STALE - ms(1)));
        assert!(r.may_adopt(t0 + STALE));
    }

    #[test]
    fn a_snapshot_is_not_adopted_while_the_knob_is_turning() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_synced(t0 - STALE);
        assert!(r.may_adopt(t0), "settled and idle");

        r.on_tick(HotkeyAction::VolumeUp, t0);
        assert!(!r.may_adopt(t0 + ms(300)), "the bar must not jump mid-turn");
        assert!(r.may_adopt(t0 + STALE));
    }

    #[test]
    fn a_snapshot_is_adopted_before_anything_has_happened() {
        let r = readback();
        // Nothing sent and nothing turned: whatever the device says is news.
        assert!(r.may_adopt(Instant::now()));
    }

    #[test]
    fn the_window_follows_the_setting() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_synced(t0);
        r.set_stale_after(Duration::from_secs(5));

        assert!(!r.may_adopt(t0 + Duration::from_secs(4)));
        assert!(r.may_adopt(t0 + Duration::from_secs(5)));
        // And the same window decides when a tick has to read back.
        assert_eq!(
            r.on_tick(HotkeyAction::VolumeUp, t0 + Duration::from_secs(4)),
            TickPlan::Send,
            "inside the window the local value is the authority"
        );
    }

    #[test]
    fn logged_out_ticks_are_sent_and_report_their_own_error() {
        let mut r = Readback::new(STALE, TIMEOUT);
        let t0 = Instant::now();
        // No reading can arrive, so holding the tick would only delay the
        // "not logged in" message the send produces.
        assert_eq!(r.on_tick(HotkeyAction::VolumeUp, t0), TickPlan::Send);
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn logging_out_drops_the_baseline_and_the_held_tick() {
        let mut r = readback();
        let t0 = Instant::now();
        r.on_synced(t0);
        r.on_tick(HotkeyAction::VolumeUp, t0 + STALE);
        assert!(r.is_pending());

        r.set_logged_in(false);
        assert!(!r.is_pending());
        // Logging back in must not trust the volume from the old session.
        r.set_logged_in(true);
        assert_eq!(
            r.on_tick(HotkeyAction::VolumeUp, t0 + STALE + ms(10)),
            TickPlan::ReadBack
        );
    }
}
