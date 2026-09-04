//! Local, instantly-updated model of the Spotify volume.
//!
//! The UI mutates this on every knob tick and shows the result immediately;
//! the Spotify actor reconciles the remote device asynchronously.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeModel {
    /// What the user currently sees, 0..=100.
    pub local: u8,
    /// Volume before muting; `Some` while muted.
    pub pre_mute: Option<u8>,
}

impl Default for VolumeModel {
    fn default() -> Self {
        Self::new(50)
    }
}

impl VolumeModel {
    pub fn new(volume: u8) -> Self {
        Self {
            local: volume.min(100),
            pre_mute: None,
        }
    }

    pub fn is_muted(&self) -> bool {
        self.pre_mute.is_some()
    }

    /// Increase by `step`, saturating at 100. Unmutes. Returns the new volume.
    pub fn step_up(&mut self, step: u8) -> u8 {
        self.pre_mute = None;
        self.local = self.local.saturating_add(step).min(100);
        self.local
    }

    /// Decrease by `step`, saturating at 0. Unmutes. Returns the new volume.
    pub fn step_down(&mut self, step: u8) -> u8 {
        self.pre_mute = None;
        self.local = self.local.saturating_sub(step);
        self.local
    }

    /// Mute (remembering the level) or restore the remembered level.
    /// Returns the new volume to send to Spotify.
    pub fn toggle_mute(&mut self) -> u8 {
        match self.pre_mute.take() {
            Some(previous) => self.local = previous,
            None => {
                self.pre_mute = Some(self.local);
                self.local = 0;
            }
        }
        self.local
    }

    /// Adopt the volume reported by the Spotify device.
    pub fn sync_remote(&mut self, remote: u8) {
        self.local = remote.min(100);
        if self.local > 0 {
            self.pre_mute = None;
        }
    }
}
