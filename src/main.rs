#![windows_subsystem = "windows"]

mod config;
mod spotify;

use std::thread;
use futures::executor;
use dotenv::dotenv;
use anyhow::{Context, Result};
use spotify::Spotify;
use tray_icon::{
    menu::{Menu, MenuItemBuilder},
    TrayIconBuilder,
    TrayIconEvent
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    window::Window,
    event_loop::{ActiveEventLoop, EventLoop}
};

enum UserEvent {
    TrayIconEvent(tray_icon::TrayIconEvent),
    MenuEvent(tray_icon::menu::MenuEvent),
    KeyPressEvent(rdev::Key)
}

#[derive(Default)]
struct KnobifyApp {
    spotify: Spotify,
    window: Option<Window>
}

impl ApplicationHandler<UserEvent> for KnobifyApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
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
            UserEvent::KeyPressEvent(key) => {
                if let rdev::Key::Unknown(code) = key {
                    if code == volume_up_key {
                        executor::block_on(self.spotify.volume_up()).unwrap();
                    } else if code == volume_down_key {
                        executor::block_on(self.spotify.volume_down()).unwrap();
                    }
                }
            },
            UserEvent::MenuEvent(event) => {
                match event.id.0.as_str() {
                    "exit" => event_loop.exit(),
                    "login" => {
                        self.spotify = executor::block_on(Spotify::login()).unwrap();
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
    dotenv().ok();

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
    let tray_menu = Menu::new();

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
        .with_tooltip("Knobify")
        .build()?;

    let proxy = event_loop.create_proxy();
    thread::spawn(move || {
        let callback = move |event: rdev::Event| {
            if let rdev::EventType::KeyPress(key) = event.event_type {
                let _ = proxy.send_event(UserEvent::KeyPressEvent(key));
            }
        };

        if let Err(error) = rdev::listen(callback) {
            eprintln!("Error in rdev listener: {:?}", error);
        }
    });

    event_loop.run_app(&mut app)?;

    Ok(())
}
