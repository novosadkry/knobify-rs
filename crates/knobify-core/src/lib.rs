//! Platform-independent core of Knobify.
//!
//! Everything in this crate compiles and is unit-tested on any host. The
//! Windows-only binary crate (`knobify`) layers the global key hook, the tray
//! icon and the egui windows on top of these types.

pub mod coalescer;
pub mod config;
pub mod events;
pub mod keycode;
pub mod spotify;
pub mod volume;

pub use config::{Bindings, OsdPosition, OsdSettings, Settings};
pub use events::{AppEvent, BindingTarget, HotkeyAction, TrayAction};
pub use keycode::KeyCode;
pub use volume::VolumeModel;
