//! Knobify: control Spotify volume with a keyboard knob.
//!
//! Windows-only. The root eframe window *is* the on-screen volume popup
//! (transparent, click-through, always on top); Settings is a child viewport;
//! a tray icon hosts the menu. See `app.rs` for the wiring.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod handle;
mod hotkeys;
mod icon;
mod ui;
mod win;

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use knobify_core::config;

const LOG_FILE_NAME: &str = "knobify.log";

/// Where log records go: the file, plus stderr in debug builds.
///
/// The release binary is built with `windows_subsystem = "windows"`, so it has
/// no console and stderr is discarded even when started from a terminal - the
/// file is the only way to see anything.
struct LogSink {
    file: File,
}

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(debug_assertions)]
        {
            let _ = io::stderr().write_all(buf);
        }
        // env_logger writes one whole record at a time; do not report a short
        // write and risk a truncated line.
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        #[cfg(debug_assertions)]
        {
            let _ = io::stderr().flush();
        }
        self.file.flush()
    }
}

/// Everything at `info`, except rspotify's HTTP layer, which logs whole
/// `RequestBuilder`s - **including the `Authorization: Bearer ...` header** - at
/// info level. That would write a live access token into a file on disk that
/// users are asked to paste when reporting problems, so it is muted to `warn`
/// here. `RUST_LOG` still overrides all of it (`RUST_LOG=debug` re-enables the
/// token logging, so prefer `RUST_LOG=knobify=debug` when troubleshooting).
const DEFAULT_LOG_FILTER: &str = "info,rspotify_http=warn";

/// Returns the log file path when one could be opened (it is truncated on
/// every start).
fn init_logging(dir: Option<&Path>) -> Option<PathBuf> {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(DEFAULT_LOG_FILTER),
    );

    let path = dir.map(|dir| dir.join(LOG_FILE_NAME));
    let opened = path.as_ref().and_then(|path| {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("knobify: cannot create {}: {e}", parent.display());
                return None;
            }
        }
        match File::create(path) {
            Ok(file) => Some(file),
            Err(e) => {
                eprintln!("knobify: cannot write {}: {e}", path.display());
                None
            }
        }
    });

    match opened {
        Some(file) => {
            builder.target(env_logger::Target::Pipe(Box::new(LogSink { file })));
            builder.init();
            path
        }
        None => {
            builder.init();
            None
        }
    }
}

fn main() -> anyhow::Result<()> {
    // The log lives next to the config, so the directory has to be resolved
    // before logging can start.
    let cfg_dir = config::config_dir();
    let log_path = init_logging(cfg_dir.as_ref().ok().map(PathBuf::as_path));

    let cfg_dir = cfg_dir.inspect_err(|e| log::error!("{e}")).context(
        // Without a console there is nothing to print this to, so it is logged
        // as well (when logging itself could be set up).
        "locating the config directory",
    )?;
    let cfg_path = cfg_dir.join(config::CONFIG_FILE_NAME);
    let token_path = cfg_dir.join(config::TOKEN_FILE_NAME);
    log::info!("knobify {} starting", env!("CARGO_PKG_VERSION"));
    log::info!("config: {}", cfg_path.display());
    if let Some(log_path) = &log_path {
        log::info!("log: {}", log_path.display());
    }
    log::info!(
        "token cache {}: {}",
        if token_path.exists() {
            "present"
        } else {
            "absent (login needed)"
        },
        token_path.display()
    );

    let settings = match config::load(&cfg_path) {
        Ok(settings) => settings,
        Err(e) => {
            log::error!("{e}; starting with default settings (the file is left untouched)");
            config::Settings::default()
        }
    };
    log::info!(
        "settings: client id {}, step {}%, popup {} at {:?}, suppress {}",
        if settings.client_id.is_empty() {
            "missing (open Settings)"
        } else {
            "set"
        },
        settings.step,
        if settings.osd.enabled {
            "enabled"
        } else {
            "disabled"
        },
        settings.osd.position,
        settings.bindings.suppress,
    );

    let native_options = eframe::NativeOptions {
        viewport: ui::osd::root_viewport_builder(&settings.osd),
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        centered: false,
        ..Default::default()
    };

    eframe::run_native(
        "Knobify",
        native_options,
        Box::new(move |cc| {
            let app = app::KnobifyApp::new(cc, settings, cfg_path)?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| {
        log::error!("eframe failed: {e}");
        anyhow!("eframe failed: {e}")
    })
}
