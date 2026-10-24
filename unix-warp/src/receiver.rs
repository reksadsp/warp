use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};

use crate::capture::CapturedFrame;

pub struct Receiver {
    latest: Arc<Mutex<Option<CapturedFrame>>>,
    running: Arc<AtomicBool>,
    child: Option<Child>,
    thread: Option<JoinHandle<()>>,
    last_taken: u64,
}

impl Receiver {
    pub fn start(url: &str, size: (u32, u32)) -> Result<Self> {
        let frame_len = size.0 as usize * size.1 as usize * 4;
        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "warning",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-i",
                url,
                "-an",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "bgra",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start ffmpeg receiver; install ffmpeg or use nix develop .#unix-warp")?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ffmpeg receiver stdout was not captured"))?;
        let latest = Arc::new(Mutex::new(None));
        let running = Arc::new(AtomicBool::new(true));
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = {
            let latest = Arc::clone(&latest);
            let running = Arc::clone(&running);
            std::thread::Builder::new()
                .name("unix-warp-receiver".into())
                .spawn(move || {
                    let mut stdout = stdout;
                    let _ = ready_tx.send(());
                    let mut sequence = 0;
                    while running.load(Ordering::Relaxed) {
                        let mut bgra = vec![0; frame_len];
                        if stdout.read_exact(&mut bgra).is_err() {
                            break;
                        }
                        sequence += 1;
                        *latest.lock().expect("receiver frame lock") = Some(CapturedFrame {
                            width: size.0,
                            height: size.1,
                            bgra,
                            sequence,
                        });
                    }
                })?
        };
        ready_rx
            .recv()
            .context("receiver thread stopped before starting")?;
        Ok(Self {
            latest,
            running,
            child: Some(child),
            thread: Some(thread),
            last_taken: 0,
        })
    }

    pub fn take_new_frame(&mut self) -> Option<CapturedFrame> {
        let mut latest = self.latest.lock().expect("receiver frame lock");
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

impl Drop for Receiver {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.thread.take();
    }
}
