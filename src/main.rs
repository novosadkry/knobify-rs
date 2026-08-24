#![windows_subsystem = "windows"]

mod config;
mod keyboard_hook;
mod spotify;

use std::{env, thread};
use futures::executor;
use dotenv::{dotenv, from_path};
use anyhow::{Context, Result};
use spotify::Spotify;
use tray_icon::{
    menu::{Menu, MenuItem, MenuItemBuilder},
    Icon,
    TrayIconBuilder,
    TrayIconEvent
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    window::Window,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop}
};

enum UserEvent {
    TrayIconEvent(tray_icon::TrayIconEvent),
    MenuEvent(tray_icon::menu::MenuEvent),
    KeyPressEvent(u32)
}

const STATUS_LOGGED_IN: &str = "● Logged in";
const STATUS_LOGGED_OUT: &str = "○ Not logged in";

#[derive(Default)]
struct KnobifyApp {
    spotify: Spotify,
    window: Option<Window>,
    status_item: Option<MenuItem>
}

impl KnobifyApp {
    fn set_logged_in(&self, logged_in: bool) {
        if let Some(status_item) = &self.status_item {
            status_item.set_text(if logged_in { STATUS_LOGGED_IN } else { STATUS_LOGGED_OUT });
        }
    }
}

impl ApplicationHandler<UserEvent> for KnobifyApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);

        let window_attributes = Window::default_attributes().with_visible(false);
        self.window = Some(event_loop.create_window(window_attributes).unwrap());
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        if let WindowEvent::CloseRequested = event {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let volume_up_key = config::get_volume_up_key();
        let volume_down_key = config::get_volume_down_key();

        match event {
            UserEvent::KeyPressEvent(code) => {
                let result = if code == volume_up_key {
                    Some(executor::block_on(self.spotify.volume_up()))
                } else if code == volume_down_key {
                    Some(executor::block_on(self.spotify.volume_down()))
                } else {
                    None
                };

                if let Some(Err(error)) = result {
                    eprintln!("Error changing Spotify volume: {:?}", error);
                }
            },
            UserEvent::MenuEvent(event) => {
                match event.id.0.as_str() {
                    "exit" => event_loop.exit(),
                    "login" => {
                        match executor::block_on(Spotify::login()) {
                            Ok(spotify) => {
                                self.spotify = spotify;
                                self.set_logged_in(true);
                            },
                            Err(error) => eprintln!("Error logging into Spotify: {:?}", error),
                        }
                    },
                    _ => {}
                }
            },
            UserEvent::TrayIconEvent(TrayIconEvent::DoubleClick { .. }) => {
                let window = self.window.as_ref().unwrap();
                window.set_visible(true);
            },
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let exe_dir = env::current_exe()
        .context("Failed to get current exe path!")?
        .parent()
        .context("Failed to get exe directory!")?
        .to_path_buf();

    // Prefer a .env next to the exe (for a distributed build); fall back to the
    // cwd-based search so `cargo run` still picks up the one in the project root.
    if from_path(exe_dir.join(".env")).is_err() {
        dotenv().ok();
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()
        .context("Failed to create event loop!")?;

    let proxy = event_loop.create_proxy();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::TrayIconEvent(event));
    }));

    let proxy = event_loop.create_proxy();
    tray_icon::menu::MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::MenuEvent(event));
    }));

    let mut app = KnobifyApp::default();

    // Silently restore a previously logged-in session, if one is cached, so
    // the user doesn't have to log in again every time the app starts.
    let logged_in = match Spotify::from_cache().await {
        Ok(spotify) => {
            app.spotify = spotify;
            true
        },
        Err(error) => {
            eprintln!("No cached Spotify session restored: {:?}", error);
            false
        },
    };

    let tray_menu = Menu::new();

    let status_item = MenuItemBuilder::new()
        .text(if logged_in { STATUS_LOGGED_IN } else { STATUS_LOGGED_OUT })
        .id("status".into())
        .enabled(false)
        .build();

    tray_menu.append(&status_item)?;
    app.status_item = Some(status_item);

    tray_menu.append(&MenuItemBuilder::new()
        .text("Login")
        .id("login".into())
        .enabled(true)
        .build())?;

    tray_menu.append(&MenuItemBuilder::new()
        .text("Exit")
        .id("exit".into())
        .enabled(true)
        .build())?;

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_icon(Icon::from_resource(1, Some((512, 512)))?)
        .with_tooltip("Knobify")
        .build()?;

    let proxy = event_loop.create_proxy();
    thread::spawn(move || {
        let callback = move |code: u32| {
            let _ = proxy.send_event(UserEvent::KeyPressEvent(code));
        };

        if let Err(error) = keyboard_hook::listen(callback) {
            eprintln!("Error in keyboard hook listener: {:?}", error);
        }
    });

    event_loop.run_app(&mut app)?;

    Ok(())
}
