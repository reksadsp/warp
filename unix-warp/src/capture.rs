use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use xcap::{Monitor, Window};

pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub sequence: u64,
}

pub enum Target {
    Window(Window),
    Display(Monitor),
}

struct SendTarget(Target);
unsafe impl Send for SendTarget {}

pub struct Capture {
    latest: Arc<Mutex<Option<CapturedFrame>>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    last_taken: u64,
}

impl Capture {
    pub fn start(target: Target, fps: u32, show_cursor: bool) -> Result<Self> {
        let latest = Arc::new(Mutex::new(None));
        let running = Arc::new(AtomicBool::new(true));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let target = SendTarget(target);
        let thread = {
            let latest = Arc::clone(&latest);
            let running = Arc::clone(&running);
            std::thread::Builder::new()
                .name("unix-warp-capture".into())
                .spawn(move || {
                    let _ = show_cursor;
                    let target = target.0;
                    let frame_interval =
                        std::time::Duration::from_secs_f64(1.0 / fps.max(1) as f64);
                    let mut sequence = 0;
                    let mut capture_frame = || match target_image(&target) {
                        Ok(image) => {
                            sequence += 1;
                            Ok(CapturedFrame {
                                width: image.width(),
                                height: image.height(),
                                bgra: rgba_to_bgra(image.into_raw()),
                                sequence,
                            })
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    match capture_frame() {
                        Ok(frame) => {
                            *latest.lock().expect("capture frame lock") = Some(frame);
                            let _ = ready_tx.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                            return;
                        }
                    }
                    while running.load(Ordering::Relaxed) {
                        let started = std::time::Instant::now();
                        if let Ok(frame) = capture_frame() {
                            *latest.lock().expect("capture frame lock") = Some(frame);
                        }
                        std::thread::sleep(frame_interval.saturating_sub(started.elapsed()));
                    }
                })?
        };

        ready_rx
            .recv()
            .context("capture thread stopped before starting")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            latest,
            running,
            thread: Some(thread),
            last_taken: 0,
        })
    }

    pub fn take_new_frame(&mut self) -> Option<CapturedFrame> {
        let mut latest = self.latest.lock().expect("capture frame lock");
        if latest
            .as_ref()
            .is_some_and(|frame| frame.sequence != self.last_taken)
        {
            let frame = latest.take()?;
            self.last_taken = frame.sequence;
            Some(frame)
        } else {
            None
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.thread.take();
    }
}

pub fn find_window(needle: &str) -> Result<Target> {
    let needle = needle.to_lowercase();
    let mut targets = Window::all()
        .context("enumerate windows")?
        .into_iter()
        .filter(|window| {
            window
                .title()
                .is_ok_and(|title| title.to_lowercase().contains(&needle))
        });
    let target = targets
        .next()
        .ok_or_else(|| anyhow!("no window title contains {needle:?}; use --list"))?;
    if targets.next().is_some() {
        eprintln!("warning: several windows matched {needle:?}; using the first");
    }
    Ok(Target::Window(target))
}

pub fn main_display() -> Result<Target> {
    let display = Monitor::all()
        .context("enumerate displays")?
        .into_iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .ok_or_else(|| anyhow!("no display target available"))?;
    Ok(Target::Display(display))
}

pub fn list_targets() {
    match Monitor::all() {
        Ok(monitors) => {
            for monitor in monitors {
                println!(
                    "display [{}] {}",
                    monitor.id().unwrap_or_default(),
                    monitor.friendly_name().unwrap_or_else(|_| "unnamed".into())
                );
            }
        }
        Err(error) => eprintln!("cannot enumerate displays: {error}"),
    }
    match Window::all() {
        Ok(windows) => {
            for window in windows {
                println!(
                    "window  [{}] {}",
                    window.id().unwrap_or_default(),
                    window.title().unwrap_or_else(|_| "untitled".into())
                );
            }
        }
        Err(error) => eprintln!("cannot enumerate windows: {error}"),
    }
}

fn target_image(target: &Target) -> Result<xcap::image::RgbaImage> {
    match target {
        Target::Window(window) => window.capture_image().map_err(Into::into),
        Target::Display(monitor) => monitor.capture_image().map_err(Into::into),
    }
}

fn rgba_to_bgra(mut pixels: Vec<u8>) -> Vec<u8> {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    pixels
}
