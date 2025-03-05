#![windows_subsystem = "windows"]

use std::thread;
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
    window: Option<Window>
}

impl ApplicationHandler<UserEvent> for KnobifyApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window_attributes = Window::default_attributes()
            .with_title("A fantastic window!")
            .with_active(false)
            .with_visible(false);

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
        match event {
            UserEvent::KeyPressEvent(key) => {
                match key {
                    rdev::Key::Alt => { event_loop.exit(); }
                    _ => {}
                }
            },
            UserEvent::MenuEvent(event) => {
                if event.id == "exit" {
                    event_loop.exit();
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

fn main() {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().unwrap();

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
        .text("Exit")
        .id("exit".into())
        .enabled(true)
        .build()).unwrap();

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Knobify")
        .build()
        .unwrap();

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

    event_loop.run_app(&mut app).unwrap();
}
