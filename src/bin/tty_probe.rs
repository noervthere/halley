use calloop::EventLoop;
use smithay::backend::drm::DrmEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::reexports::input::Libinput;

// src/bin/*.rs binaries are separate crates from main.rs and can't import
// its modules directly - reuse the same source tree via #[path] rather than
// touching main.rs (out of scope for this plan) to share it.
#[path = "../backend/mod.rs"]
mod backend;
#[path = "../cursor.rs"]
mod cursor;
#[path = "../input/mod.rs"]
mod input;

use backend::CLEAR_COLOR;
use backend::Renderable;
use backend::tty::TtyBackend;
use cursor::CursorImage;
use input::Keyboard;
use input::keybinds::BackendKind;

/// Mirrors main.rs's `App` shape (backend + keyboard + cursor + exit flag).
/// Still a separate struct in a separate binary; full winit/tty unification
/// is later, explicitly-deferred work.
struct TtyApp {
    backend: TtyBackend,
    keyboard: Keyboard,
    cursor: CursorImage,
    exit: bool,
}

fn main() {
    let (backend, session_notifier, drm_notifier) = match TtyBackend::new() {
        Ok(parts) => parts,
        // Expected nested under a host compositor (niri already holds
        // exclusive session control) - confirmed since step 3. Real success
        // needs a free VT.
        Err(err) => {
            println!("TtyBackend::new() failed: {err}");
            return;
        }
    };
    println!("TtyBackend constructed successfully");

    let mut libinput_context = Libinput::new_with_udev::<LibinputSessionInterface<_>>(
        backend.session().into(),
    );
    libinput_context
        .udev_assign_seat(&backend.session().seat())
        .expect("failed to assign udev seat for libinput");
    let libinput_backend = LibinputInputBackend::new(libinput_context);

    let mut app = TtyApp {
        backend,
        keyboard: Keyboard::new(BackendKind::Tty).expect("failed to set up keyboard input"),
        cursor: CursorImage::load(),
        exit: false,
    };

    match app.backend.render(CLEAR_COLOR, &app.cursor) {
        Ok(()) => println!("first render() succeeded"),
        Err(err) => println!("first render() failed (same caveat as initialize_output): {err}"),
    }

    let mut event_loop: EventLoop<TtyApp> = EventLoop::try_new().expect("failed to create event loop");

    event_loop
        .handle()
        .insert_source(libinput_backend, |event, _, app| {
            if let Some(halley_config::Action::Quit) = app.keyboard.process_input_event(event) {
                app.exit = true;
            }
        })
        .expect("failed to insert libinput source");

    event_loop
        .handle()
        .insert_source(session_notifier, |event, _, app| match event {
            SessionEvent::PauseSession => {
                println!("session event: pause");
                app.backend.pause();
            }
            SessionEvent::ActivateSession => {
                println!("session event: activate");
                match app.backend.resume() {
                    Ok(()) => {
                        if let Err(err) = app.backend.render(CLEAR_COLOR, &app.cursor) {
                            println!("post-resume render failed: {err}");
                        }
                    }
                    Err(err) => println!("resume failed: {err}"),
                }
            }
        })
        .expect("failed to insert session notifier");

    event_loop
        .handle()
        .insert_source(drm_notifier, |event, _, app| match event {
            DrmEvent::VBlank(crtc) => {
                if let Err(err) = app.backend.frame_submitted(crtc) {
                    println!("frame_submitted failed for {crtc:?}: {err}");
                    return;
                }
                if let Err(err) = app.backend.render(CLEAR_COLOR, &app.cursor) {
                    println!("render failed: {err}");
                }
            }
            DrmEvent::Error(err) => println!("drm event: error {err:?}"),
        })
        .expect("failed to insert drm notifier");

    println!("dispatching - switch to this VT to see a solid color fill the screen, press the Quit chord to exit");
    while !app.exit {
        event_loop
            .dispatch(None, &mut app)
            .expect("event loop dispatch failed");
    }
    println!("quit requested, exiting cleanly");
}
