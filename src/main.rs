mod backend;

use calloop::EventLoop;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;

use backend::winit::WinitBackend;

/// Everything this milestone needs: a backend to render into, and whether
/// we should stop. Deliberately not the old `Halley` mega-struct - grows
/// exactly when a real feature (xdg-shell, input, ...) needs it to.
struct App {
    #[allow(dead_code)] // read starting next commit, when Redraw actually renders
    backend: WinitBackend,
    exit: bool,
}

fn main() {
    let window_attributes = WinitWindow::default_attributes()
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_title("halley-wl");

    let (backend, winit_source) =
        smithay_winit::init_from_attributes::<GlesRenderer>(window_attributes)
            .expect("failed to initialize winit backend");

    let mut event_loop: EventLoop<App> =
        EventLoop::try_new().expect("failed to create event loop");

    let mut app = App {
        backend: WinitBackend::new(backend),
        exit: false,
    };

    event_loop
        .handle()
        .insert_source(winit_source, move |event, _, app| match event {
            WinitEvent::CloseRequested => {
                app.exit = true;
            }
            WinitEvent::Redraw => {
                // TODO: render (next commit).
            }
            WinitEvent::Resized { .. } => {
                // TODO: track new size and request a redraw (later commit).
            }
            _ => {}
        })
        .expect("failed to insert winit event source");

    println!("halley-wl starting");

    while !app.exit {
        event_loop
            .dispatch(None, &mut app)
            .expect("event loop dispatch failed");
    }
}
