//! Thread-safe, bounded state shared by the Warp session worker and UI.

use super::{Capture, Connection, Identity, Snapshot};
use std::collections::VecDeque;
use std::time::Duration;

pub const EVENT_LOG_LIMIT: usize = 256;
pub const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cli {
    pub device: String,
    pub host_id: String,
    pub capture_screen: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            device: "127.0.0.1:47052".into(),
            host_id: "warp".into(),
            capture_screen: false,
        }
    }
}

impl Cli {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut cli = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--device" => cli.device = args.next().ok_or("--device requires host:port")?,
                "--host-id" => cli.host_id = args.next().ok_or("--host-id requires a name")?,
                "--capture-screen" => cli.capture_screen = true,
                "--help" | "-h" => {
                    return Err(
                        "usage: warp [--device host:port] [--host-id name] [--capture-screen]"
                            .into(),
                    );
                }
                _ => return Err(format!("unknown argument: {arg}")),
            }
        }
        if cli.device.is_empty() || cli.host_id.is_empty() || cli.host_id.len() > 128 {
            return Err(
                "device and host ID must be non-empty; host ID is at most 128 bytes".into(),
            );
        }
        Ok(cli)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionUi {
    Disconnected,
    Connecting,
    Active(Identity),
    Rejected(u32),
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub session: SessionUi,
    pub snapshot: Option<Snapshot>,
    pub capture: Option<Capture>,
    pub events: VecDeque<String>,
    pub dirty: bool,
    pub retry_attempt: usize,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            session: SessionUi::Disconnected,
            snapshot: None,
            capture: None,
            events: VecDeque::new(),
            dirty: true,
            retry_attempt: 0,
        }
    }
}

impl AppState {
    pub fn record(&mut self, event: impl Into<String>) {
        let event = event.into();
        println!("{event}");
        if self.events.len() == EVENT_LOG_LIMIT {
            self.events.pop_front();
        }
        self.events.push_back(event);
        self.dirty = true;
    }

    pub fn set_connection(&mut self, connection: Connection, identity: Option<Identity>) {
        self.session = match (connection, identity) {
            (Connection::Active, Some(identity)) => {
                self.retry_attempt = 0;
                SessionUi::Active(identity)
            }
            (Connection::Handshaking, _) => SessionUi::Connecting,
            _ => SessionUi::Disconnected,
        };
        self.dirty = true;
    }

    pub fn reject(&mut self, reason: u32) {
        self.session = SessionUi::Rejected(reason);
        self.dirty = true;
    }

    pub fn next_retry_delay(&mut self) -> Duration {
        let delay = RETRY_DELAYS[self.retry_attempt.min(RETRY_DELAYS.len() - 1)];
        self.retry_attempt = (self.retry_attempt + 1).min(RETRY_DELAYS.len() - 1);
        delay
    }
}
