//! Warp Link v1 client codec and reconnecting trusted-LAN session.

#[path = "app_state.rs"]
pub mod app_state;

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub const VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 16;
pub const MAX_CONTROL: usize = 4 * 1024;
pub const MAX_CHUNK: usize = 60 * 1024;
pub const MAX_CAPTURE: usize = 512 * 1024;
pub const CAP_STATE: u32 = 1;
pub const CAP_CAPTURE: u32 = 2;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Type {
    Hello = 1,
    HelloAck,
    Reject,
    Heartbeat,
    StateRequest,
    StateSnapshot,
    CaptureRequest,
    CaptureBegin,
    CaptureChunk,
    CaptureEnd,
    Request,
    Outcome,
    Disconnect,
}
impl TryFrom<u8> for Type {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Reject),
            4 => Ok(Self::Heartbeat),
            5 => Ok(Self::StateRequest),
            6 => Ok(Self::StateSnapshot),
            7 => Ok(Self::CaptureRequest),
            8 => Ok(Self::CaptureBegin),
            9 => Ok(Self::CaptureChunk),
            10 => Ok(Self::CaptureEnd),
            11 => Ok(Self::Request),
            12 => Ok(Self::Outcome),
            13 => Ok(Self::Disconnect),
            _ => Err(Error::UnknownType),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: Type,
    pub correlation_id: u32,
    pub payload: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    Magic,
    Version,
    Flags,
    HeaderSize,
    UnknownType,
    Oversized,
    RequiredField,
    InvalidCapture,
    Io,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connection {
    Disconnected,
    Handshaking,
    Active,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub session_id: u32,
    pub device_id: String,
    pub software_version: String,
    pub capabilities: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub timestamp_ms: u64,
    pub connection: Connection,
    pub capabilities: u32,
    pub device_id: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn encode(frame: &Frame) -> Result<Vec<u8>, Error> {
    let limit = if frame.kind == Type::CaptureChunk {
        MAX_CHUNK
    } else {
        MAX_CONTROL
    };
    if frame.payload.len() > limit {
        return Err(Error::Oversized);
    }
    let mut out = Vec::with_capacity(HEADER_SIZE + frame.payload.len());
    out.extend_from_slice(b"RX");
    out.extend_from_slice(&[VERSION, frame.kind as u8]);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
    out.extend_from_slice(&(frame.payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&frame.correlation_id.to_be_bytes());
    out.extend_from_slice(&frame.payload);
    Ok(out)
}
pub fn decode(bytes: &[u8]) -> Result<Frame, Error> {
    if bytes.len() < HEADER_SIZE {
        return Err(Error::Truncated);
    }
    if &bytes[0..2] != b"RX" {
        return Err(Error::Magic);
    }
    if bytes[2] != VERSION {
        return Err(Error::Version);
    }
    let kind = Type::try_from(bytes[3])?;
    if u16::from_be_bytes([bytes[4], bytes[5]]) != 0 {
        return Err(Error::Flags);
    }
    if u16::from_be_bytes([bytes[6], bytes[7]]) as usize != HEADER_SIZE {
        return Err(Error::HeaderSize);
    }
    let size = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let limit = if kind == Type::CaptureChunk {
        MAX_CHUNK
    } else {
        MAX_CONTROL
    };
    if size > limit {
        return Err(Error::Oversized);
    }
    if bytes.len() != HEADER_SIZE + size {
        return Err(Error::Truncated);
    }
    Ok(Frame {
        kind,
        correlation_id: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
        payload: bytes[HEADER_SIZE..].to_vec(),
    })
}

// ---------- Payload codecs (mirroring the embedded device) ----------

pub const MAX_ID: usize = 128;
pub const REJECT_ALREADY_CONNECTED: u32 = 1;
pub const REJECT_INCOMPATIBLE_VERSION: u32 = 2;
pub const REJECT_MALFORMED_HELLO: u32 = 3;

fn read_u16(payload: &[u8], offset: usize) -> Result<u16, Error> {
    payload
        .get(offset..offset + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or(Error::RequiredField)
}
fn read_u32(payload: &[u8], offset: usize) -> Result<u32, Error> {
    payload
        .get(offset..offset + 4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        .ok_or(Error::RequiredField)
}
fn read_string(payload: &[u8], offset: usize) -> Result<(String, usize), Error> {
    let length = read_u16(payload, offset)? as usize;
    if length > MAX_ID {
        return Err(Error::RequiredField);
    }
    let end = offset + 2 + length;
    let bytes = payload.get(offset + 2..end).ok_or(Error::RequiredField)?;
    Ok((String::from_utf8_lossy(bytes).into(), end))
}

/// Host identity offered in `Hello`: `[id_len u16][id][capabilities u32]`.
pub fn encode_hello(host_id: &str, capabilities: u32) -> Result<Vec<u8>, Error> {
    if host_id.is_empty() || host_id.len() > MAX_ID {
        return Err(Error::RequiredField);
    }
    let mut payload = Vec::with_capacity(2 + host_id.len() + 4);
    payload.extend_from_slice(&(host_id.len() as u16).to_be_bytes());
    payload.extend_from_slice(host_id.as_bytes());
    payload.extend_from_slice(&capabilities.to_be_bytes());
    Ok(payload)
}

/// Device identity granted in `HelloAck`:
/// `[session u32][id_len u16][id][version_len u16][version][capabilities u32]`.
pub fn decode_hello_ack(payload: &[u8]) -> Result<Identity, Error> {
    let session_id = read_u32(payload, 0)?;
    if session_id == 0 {
        return Err(Error::RequiredField);
    }
    let (device_id, offset) = read_string(payload, 4)?;
    if device_id.is_empty() {
        return Err(Error::RequiredField);
    }
    let (software_version, offset) = read_string(payload, offset)?;
    if software_version.is_empty() {
        return Err(Error::RequiredField);
    }
    Ok(Identity {
        session_id,
        device_id,
        software_version,
        capabilities: read_u32(payload, offset)?,
    })
}
pub fn encode_hello_ack(identity: &Identity) -> Result<Vec<u8>, Error> {
    if identity.session_id == 0
        || identity.device_id.is_empty()
        || identity.software_version.is_empty()
        || identity.device_id.len() > MAX_ID
        || identity.software_version.len() > MAX_ID
    {
        return Err(Error::RequiredField);
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(&identity.session_id.to_be_bytes());
    payload.extend_from_slice(&(identity.device_id.len() as u16).to_be_bytes());
    payload.extend_from_slice(identity.device_id.as_bytes());
    payload.extend_from_slice(&(identity.software_version.len() as u16).to_be_bytes());
    payload.extend_from_slice(identity.software_version.as_bytes());
    payload.extend_from_slice(&identity.capabilities.to_be_bytes());
    Ok(payload)
}

/// Rejection reason code.
pub fn encode_reject(reason: u32) -> Vec<u8> {
    reason.to_be_bytes().to_vec()
}
pub fn decode_reject(payload: &[u8]) -> Result<u32, Error> {
    read_u32(payload, 0)
}

/// Device state snapshot: `[timestamp_ms u64][connection u8][capabilities u32][id_len u16][id]`.
pub fn decode_snapshot(payload: &[u8]) -> Result<Snapshot, Error> {
    let high = read_u32(payload, 0)? as u64;
    let low = read_u32(payload, 4)? as u64;
    let connection = match payload.get(8) {
        Some(0) => Connection::Disconnected,
        Some(1) => Connection::Handshaking,
        Some(2) => Connection::Active,
        _ => return Err(Error::RequiredField),
    };
    let capabilities = read_u32(payload, 9)?;
    let (device_id, _) = read_string(payload, 13)?;
    Ok(Snapshot {
        timestamp_ms: (high << 32) | low,
        connection,
        capabilities,
        device_id,
    })
}

pub struct Client {
    stream: Option<TcpStream>,
    pub state: Connection,
    pub identity: Option<Identity>,
    pub last_reject: Option<u32>,
    pub last_snapshot: Option<Snapshot>,
    next_id: u32,
    pending: HashMap<u32, Instant>,
    last_seen: Instant,
    last_heartbeat: Instant,
    capture: Option<Capture>,
    received: usize,
}
impl Client {
    pub fn new() -> Self {
        Self {
            stream: None,
            state: Connection::Disconnected,
            identity: None,
            last_reject: None,
            last_snapshot: None,
            next_id: 1,
            pending: HashMap::new(),
            last_seen: Instant::now(),
            last_heartbeat: Instant::now(),
            capture: None,
            received: 0,
        }
    }
    pub fn connect(
        &mut self,
        address: &str,
        host_id: &str,
        capabilities: u32,
    ) -> Result<(), Error> {
        let stream = TcpStream::connect(address).map_err(|_| Error::Io)?;
        stream.set_nonblocking(true).map_err(|_| Error::Io)?;
        self.stream = Some(stream);
        self.state = Connection::Handshaking;
        self.identity = None;
        self.last_reject = None;
        let payload = encode_hello(host_id, capabilities)?;
        let correlation_id = self.request_id();
        self.send(Frame {
            kind: Type::Hello,
            correlation_id,
            payload,
        })
    }
    fn request_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }
    fn send(&mut self, frame: Frame) -> Result<(), Error> {
        let bytes = encode(&frame)?;
        self.stream
            .as_mut()
            .ok_or(Error::Io)?
            .write_all(&bytes)
            .map_err(|_| Error::Io)
    }
    pub fn request_state(&mut self) -> Result<u32, Error> {
        let id = self.request_id();
        self.pending.insert(id, Instant::now());
        self.send(Frame {
            kind: Type::StateRequest,
            correlation_id: id,
            payload: vec![1],
        })?;
        Ok(id)
    }
    pub fn request_capture(&mut self) -> Result<u32, Error> {
        let id = self.request_id();
        self.pending.insert(id, Instant::now());
        self.send(Frame {
            kind: Type::CaptureRequest,
            correlation_id: id,
            payload: vec![1],
        })?;
        Ok(id)
    }
    pub fn poll(&mut self) -> Result<Vec<Frame>, Error> {
        let mut result = Vec::new();
        loop {
            let read = {
                let Some(stream) = self.stream.as_mut() else {
                    return Ok(result);
                };
                let mut header = [0; HEADER_SIZE];
                match stream.read_exact(&mut header) {
                    Ok(()) => {
                        let size = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
                        if size > MAX_CHUNK {
                            return Err(Error::Oversized);
                        }
                        let mut bytes = header.to_vec();
                        bytes.resize(HEADER_SIZE + size, 0);
                        stream
                            .read_exact(&mut bytes[HEADER_SIZE..])
                            .map_err(|_| Error::Io)?;
                        Ok(Some(bytes))
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(Error::Truncated),
                    Err(_) => Err(Error::Io),
                }
            };
            match read {
                Ok(Some(bytes)) => {
                    let frame = decode(&bytes)?;
                    self.last_seen = Instant::now();
                    self.handle(&frame)?;
                    result.push(frame);
                }
                Ok(None) => break,
                Err(Error::Truncated) => {
                    self.disconnect();
                    break;
                }
                Err(error) => {
                    self.disconnect();
                    return Err(error);
                }
            };
        }
        self.pending
            .retain(|_, instant| instant.elapsed() < Duration::from_secs(1));
        if self.state == Connection::Active {
            if self.last_heartbeat.elapsed() >= Duration::from_secs(1) {
                self.last_heartbeat = Instant::now();
                self.send(Frame {
                    kind: Type::Heartbeat,
                    correlation_id: 0,
                    payload: Vec::new(),
                })?;
            }
            if self.last_seen.elapsed() > Duration::from_secs(3) {
                self.disconnect();
            }
        }
        Ok(result)
    }
    fn handle(&mut self, frame: &Frame) -> Result<(), Error> {
        match frame.kind {
            Type::HelloAck if self.state == Connection::Handshaking => {
                self.identity = Some(decode_hello_ack(&frame.payload)?);
                self.state = Connection::Active;
                self.last_seen = Instant::now();
                self.last_heartbeat = Instant::now();
            }
            Type::Reject => {
                self.last_reject = Some(decode_reject(&frame.payload)?);
                self.disconnect();
            }
            Type::StateSnapshot => {
                self.pending.remove(&frame.correlation_id);
                if let Ok(snapshot) = decode_snapshot(&frame.payload) {
                    self.last_snapshot = Some(snapshot);
                }
            }
            Type::Outcome => {
                self.pending.remove(&frame.correlation_id);
            }
            Type::CaptureBegin if frame.payload.len() == 12 => {
                let width = u32::from_be_bytes(frame.payload[0..4].try_into().unwrap());
                let height = u32::from_be_bytes(frame.payload[4..8].try_into().unwrap());
                let size = u32::from_be_bytes(frame.payload[8..12].try_into().unwrap()) as usize;
                if width == 0 || height == 0 || width > 320 || height > 320 || size > MAX_CAPTURE {
                    return Err(Error::InvalidCapture);
                }
                self.capture = Some(Capture {
                    width,
                    height,
                    rgba: Vec::with_capacity(size),
                });
                self.received = 0;
            }
            Type::CaptureChunk => {
                if let Some(capture) = &mut self.capture {
                    if self.received + frame.payload.len() > MAX_CAPTURE {
                        return Err(Error::InvalidCapture);
                    }
                    capture.rgba.extend_from_slice(&frame.payload);
                    self.received += frame.payload.len();
                }
            }
            Type::CaptureEnd => {
                self.pending.remove(&frame.correlation_id);
            }
            _ => {}
        }
        Ok(())
    }
    pub fn take_capture(&mut self) -> Option<Capture> {
        self.capture
            .take()
            .filter(|capture| capture.rgba.len() == self.received)
    }
    pub fn disconnect(&mut self) {
        self.stream.take();
        self.state = Connection::Disconnected;
        self.identity = None;
        self.last_snapshot = None;
        self.pending.clear();
        self.capture = None;
        self.received = 0;
    }
}
