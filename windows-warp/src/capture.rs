use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{anyhow, Context, Result};
use scap::capturer::{Capturer, Options};
use scap::frame::{Frame, FrameType};
use scap::Target;

/// A BGRA frame with a tightly packed `width * height * 4` buffer.
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub sequence: u64,
}

/// A capture target only holds a window handle, which is safe to move to the
/// capture thread even though the `scap` type is not marked as such.
struct SendTarget(Target);
unsafe impl Send for SendTarget {}

impl SendTarget {
    fn into_inner(self) -> Target {
        self.0
    }
}

/// Background capture thread that always keeps the most recent frame available.
pub struct Capture {
    latest: Arc<Mutex<Option<CapturedFrame>>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    last_taken: u64,
}

impl Capture {
    pub fn start(target: Target, fps: u32, show_cursor: bool) -> Result<Self> {
        if !scap::is_supported() {
            return Err(anyhow!(
                "screen capture is not supported on this system (Windows 10 1903+ required)"
            ));
        }
        if !scap::has_permission() && !scap::request_permission() {
            return Err(anyhow!("screen capture permission was denied"));
        }

        let latest: Arc<Mutex<Option<CapturedFrame>>> = Arc::new(Mutex::new(None));
        let running = Arc::new(AtomicBool::new(true));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let target = SendTarget(target);

        let thread = {
            let latest = Arc::clone(&latest);
            let running = Arc::clone(&running);
            std::thread::Builder::new()
                .name("scap-capture".into())
                .spawn(move || {
                    // Moving the whole wrapper keeps the capture `Send`; the
                    // capturer itself is neither `Send` nor `Sync`, so it is
                    // built and torn down entirely on this thread.
                    let target = target.into_inner();
                    let options = Options {
                        fps,
                        show_cursor,
                        show_highlight: false,
                        target: Some(target),
                        output_type: FrameType::BGRAFrame,
                        ..Default::default()
                    };
                    let mut capturer = match Capturer::build(options) {
                        Ok(capturer) => {
                            let _ = ready_tx.send(Ok(()));
                            capturer
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    capturer.start_capture();
                    let mut sequence = 0u64;
                    while running.load(Ordering::Relaxed) {
                        let frame = match capturer.get_next_frame() {
                            Ok(frame) => frame,
                            Err(_) => break,
                        };
                        if let Frame::BGRA(frame) = frame {
                            if frame.width <= 0 || frame.height <= 0 {
                                continue;
                            }
                            sequence += 1;
                            let captured = CapturedFrame {
                                width: frame.width as u32,
                                height: frame.height as u32,
                                bgra: frame.data,
                                sequence,
                            };
                            *latest.lock().unwrap() = Some(captured);
                        }
                    }
                    capturer.stop_capture();
                })?
        };

        ready_rx
            .recv()
            .context("the capture thread stopped before starting")?
            .map_err(|error| anyhow!(error))
            .context("failed to build the capturer")?;

        Ok(Self {
            latest,
            running,
            thread: Some(thread),
            last_taken: 0,
        })
    }

    /// Returns the newest frame, or `None` when nothing changed since the last call.
    pub fn take_new_frame(&mut self) -> Option<CapturedFrame> {
        let mut guard = self.latest.lock().unwrap();
        match guard.as_ref() {
            Some(frame) if frame.sequence != self.last_taken => {
                let frame = guard.take()?;
                self.last_taken = frame.sequence;
                Some(frame)
            }
            _ => None,
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // `get_next_frame` blocks until the next frame arrives, so the thread is
        // left to wind down on its own rather than joined here.
        self.thread.take();
    }
}

/// Finds a capture target whose title contains `needle`, case insensitively.
pub fn find_window(needle: &str) -> Result<Target> {
    let needle = needle.to_lowercase();
    let targets = scap::get_all_targets();

    let mut matches = targets.into_iter().filter(|target| match target {
        Target::Window(window) => window.title.to_lowercase().contains(&needle),
        Target::Display(_) => false,
    });

    let first = matches
        .next()
        .ok_or_else(|| anyhow!("no window title contains {needle:?}; try --list"))?;

    if let (Target::Window(first), Some(Target::Window(second))) = (&first, matches.next()) {
        eprintln!(
            "warning: {:?} matches several windows, using {:?} (also matched {:?})",
            needle, first.title, second.title
        );
    }

    Ok(first)
}

pub fn list_targets() {
    for target in scap::get_all_targets() {
        match target {
            Target::Window(window) => println!("window  [{}] {}", window.id, window.title),
            Target::Display(display) => println!("display [{}] {}", display.id, display.title),
        }
    }
}

pub fn main_display() -> Result<Target> {
    scap::get_all_targets()
        .into_iter()
        .find(|target| matches!(target, Target::Display(_)))
        .ok_or_else(|| anyhow!("no display target available"))
}
