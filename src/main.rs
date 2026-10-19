//! Warp's nonblocking trusted-LAN session UI.

mod rexek_link;

use rexek_link::app_state::{AppState, Cli, SessionUi};
use rexek_link::{CAP_CAPTURE, CAP_STATE, Client, Connection};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

fn state_title(state: &AppState) -> String {
    let session = match &state.session {
        SessionUi::Disconnected => "disconnected".to_owned(),
        SessionUi::Connecting => "connecting".to_owned(),
        SessionUi::Rejected(reason) => format!("rejected ({reason}); press R to retry"),
        SessionUi::Active(identity) => format!(
            "active: {} {} (capabilities 0x{:x})",
            identity.device_id, identity.software_version, identity.capabilities
        ),
    };
    format!("Warp Link — {session} — trusted LAN only; no authentication or encryption")
}

fn session_worker(cli: Cli, state: Arc<Mutex<AppState>>) {
    let mut client = Client::new();
    let mut next_connect = Instant::now();
    let mut was_active = false;
    loop {
        if client.state == Connection::Disconnected && Instant::now() >= next_connect {
            if let Ok(mut ui) = state.lock() {
                if matches!(ui.session, SessionUi::Rejected(_)) {
                    // A rejection is an operator-visible compatibility / ownership error.
                    drop(ui);
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                ui.set_connection(Connection::Handshaking, None);
                ui.record(format!("Warp Link connecting to {}", cli.device));
            }
            match client.connect(&cli.device, &cli.host_id, CAP_STATE | CAP_CAPTURE) {
                Ok(()) => was_active = false,
                Err(_) => {
                    let delay = state
                        .lock()
                        .map(|mut ui| {
                            ui.set_connection(Connection::Disconnected, None);
                            let delay = ui.next_retry_delay();
                            ui.record(format!("Warp Link retrying in {} ms", delay.as_millis()));
                            delay
                        })
                        .unwrap_or(Duration::from_secs(5));
                    next_connect = Instant::now() + delay;
                }
            }
        }

        match client.poll() {
            Ok(_) => {}
            Err(error) => {
                if let Ok(mut ui) = state.lock() {
                    ui.record(format!("Warp Link transport error: {error:?}"));
                }
                client.disconnect();
            }
        }

        if let Some(reason) = client.last_reject {
            if let Ok(mut ui) = state.lock() {
                ui.reject(reason);
                ui.record(format!(
                    "Warp Link rejected by device: reason {reason}; manual retry required"
                ));
            }
            client.last_reject = None;
        } else if client.state == Connection::Active {
            if !was_active {
                was_active = true;
                let identity = client.identity.clone();
                if let Ok(mut ui) = state.lock() {
                    ui.set_connection(Connection::Active, identity);
                    ui.record("Warp Link active");
                }
                let _ = client.request_state();
                let _ = client.request_capture();
            }
            if let Ok(mut ui) = state.lock() {
                if let Some(snapshot) = client.last_snapshot.clone() {
                    ui.snapshot = Some(snapshot);
                    ui.dirty = true;
                }
                if let Some(capture) = client.take_capture() {
                    ui.capture = Some(capture);
                    ui.dirty = true;
                }
            }
        } else if was_active {
            was_active = false;
            let delay = state
                .lock()
                .map(|mut ui| {
                    ui.set_connection(Connection::Disconnected, None);
                    let delay = ui.next_retry_delay();
                    ui.record(format!(
                        "Warp Link disconnected; retrying in {} ms",
                        delay.as_millis()
                    ));
                    delay
                })
                .unwrap_or(Duration::from_secs(5));
            next_connect = Instant::now() + delay;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn main() {
    let cli = match Cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{message}");
            return;
        }
    };
    eprintln!(
        "Warp Link v1 is trusted-LAN only: it provides neither authentication nor encryption."
    );
    if cli.capture_screen {
        eprintln!(
            "--capture-screen is accepted, but screen capture is not part of the link session UI yet."
        );
    }

    let state = Arc::new(Mutex::new(AppState::default()));
    let worker_state = Arc::clone(&state);
    thread::spawn(move || session_worker(cli, worker_state));

    let event_loop = EventLoop::new().expect("create event loop");
    let window = WindowBuilder::new()
        .with_title("Warp Link — connecting")
        .with_inner_size(winit::dpi::PhysicalSize::new(800, 180))
        .build(&event_loop)
        .expect("create Warp Link window");

    event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(100)));
        match event {
            Event::AboutToWait => {
                if let Ok(mut ui) = state.lock() {
                    if ui.dirty {
                        window.set_title(&state_title(&ui));
                        ui.dirty = false;
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::KeyboardInput { event, .. }, .. }
                if event.state == winit::event::ElementState::Pressed
                    && matches!(event.logical_key, winit::keyboard::Key::Character(ref key) if key.eq_ignore_ascii_case("r")) =>
            {
                if let Ok(mut ui) = state.lock() {
                    ui.session = SessionUi::Disconnected;
                    ui.retry_attempt = 0;
                    ui.record("Warp Link manual retry requested");
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => elwt.exit(),
            _ => {}
        }
    }).expect("run event loop");
}
