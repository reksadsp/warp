//! Trusted-LAN pointer-control listener for the embedded Warp sidecar.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

use anyhow::{Context, Result, anyhow};

#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSE_EVENT_FLAGS,
    MOUSEINPUT, SendInput,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SetCursorPos, SM_CXSCREEN, SM_CYSCREEN,
};

pub const MAGIC: [u8; 2] = *b"WS";
pub const VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 16;
pub const MAX_PAYLOAD: usize = 4096;
pub const POINTER_CAPABILITY_FLAG: u32 = 0x1;
pub const REASON_OK: u16 = 0;
pub const REASON_BUSY: u16 = 1;
pub const REASON_INVALID_MESSAGE: u16 = 2;
pub const REASON_UNSUPPORTED_BUTTON: u16 = 3;
pub const REASON_OUT_OF_ORDER: u16 = 4;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageType {
    Hello = 1,
    HelloAck = 2,
    Reject = 3,
    Heartbeat = 4,
    PointerMove = 5,
    PointerButton = 6,
    Outcome = 7,
    Disconnect = 8,
}

impl TryFrom<u8> for MessageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Reject),
            4 => Ok(Self::Heartbeat),
            5 => Ok(Self::PointerMove),
            6 => Ok(Self::PointerButton),
            7 => Ok(Self::Outcome),
            8 => Ok(Self::Disconnect),
            _ => Err(anyhow!("unknown message type {value}")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub kind: MessageType,
    pub sequence: u32,
    pub reserved: u32,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hello {
    pub display_width: u16,
    pub display_height: u16,
    pub pointer_flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NormalizedPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerButton {
    None = 0,
    Primary = 1,
    Secondary = 2,
    Middle = 3,
}

impl TryFrom<u8> for PointerButton {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Primary),
            2 => Ok(Self::Secondary),
            3 => Ok(Self::Middle),
            _ => Err(anyhow!("unsupported pointer button {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerButtonAction {
    pub point: NormalizedPoint,
    pub button: PointerButton,
    pub pressed: bool,
}

pub trait PointerSink {
    fn move_pointer(&mut self, point: NormalizedPoint) -> Result<()>;
    fn button_pointer(&mut self, action: PointerButtonAction) -> Result<()>;
}

pub fn encode(message: &Message) -> Vec<u8> {
    let payload_len = message.payload.len().min(MAX_PAYLOAD) as u32;
    let mut bytes = Vec::with_capacity(HEADER_SIZE + payload_len as usize);
    bytes.extend_from_slice(&MAGIC);
    bytes.push(VERSION);
    bytes.push(message.kind as u8);
    bytes.extend_from_slice(&payload_len.to_be_bytes());
    bytes.extend_from_slice(&message.sequence.to_be_bytes());
    bytes.extend_from_slice(&message.reserved.to_be_bytes());
    bytes.extend_from_slice(&message.payload[..payload_len as usize]);
    bytes
}

pub fn decode(bytes: &[u8]) -> Result<Message> {
    if bytes.len() < HEADER_SIZE {
        anyhow::bail!("truncated control header");
    }
    if bytes[0..2] != MAGIC {
        anyhow::bail!("invalid control magic");
    }
    if bytes[2] != VERSION {
        anyhow::bail!("unsupported control version {}", bytes[2]);
    }
    let payload_len = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        anyhow::bail!("oversized control payload {payload_len}");
    }
    if bytes.len() != HEADER_SIZE + payload_len {
        anyhow::bail!("truncated control payload");
    }
    Ok(Message {
        kind: MessageType::try_from(bytes[3])?,
        sequence: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
        reserved: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
        payload: bytes[HEADER_SIZE..].to_vec(),
    })
}

pub fn encode_hello(hello: Hello) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8);
    payload.extend_from_slice(&hello.display_width.to_be_bytes());
    payload.extend_from_slice(&hello.display_height.to_be_bytes());
    payload.extend_from_slice(&hello.pointer_flags.to_be_bytes());
    payload
}

pub fn decode_hello(payload: &[u8]) -> Result<Hello> {
    if payload.len() != 8 {
        anyhow::bail!("hello payload must be 8 bytes");
    }
    Ok(Hello {
        display_width: u16::from_be_bytes(payload[0..2].try_into().unwrap()),
        display_height: u16::from_be_bytes(payload[2..4].try_into().unwrap()),
        pointer_flags: u32::from_be_bytes(payload[4..8].try_into().unwrap()),
    })
}

pub fn encode_pointer_move(point: NormalizedPoint) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4);
    payload.extend_from_slice(&point.x.to_be_bytes());
    payload.extend_from_slice(&point.y.to_be_bytes());
    payload
}

pub fn decode_pointer_move(payload: &[u8]) -> Result<NormalizedPoint> {
    if payload.len() != 4 {
        anyhow::bail!("pointer move payload must be 4 bytes");
    }
    Ok(NormalizedPoint {
        x: u16::from_be_bytes(payload[0..2].try_into().unwrap()),
        y: u16::from_be_bytes(payload[2..4].try_into().unwrap()),
    })
}

pub fn encode_pointer_button(action: PointerButtonAction) -> Vec<u8> {
    let mut payload = encode_pointer_move(action.point);
    payload.push(action.button as u8);
    payload.push(u8::from(action.pressed));
    payload
}

pub fn decode_pointer_button(payload: &[u8]) -> Result<PointerButtonAction> {
    if payload.len() != 6 {
        anyhow::bail!("pointer button payload must be 6 bytes");
    }
    Ok(PointerButtonAction {
        point: decode_pointer_move(&payload[..4])?,
        button: PointerButton::try_from(payload[4])?,
        pressed: match payload[5] {
            0 => false,
            1 => true,
            value => anyhow::bail!("invalid pointer phase {value}"),
        },
    })
}

pub fn encode_outcome(acknowledged_sequence: u32, reason_code: u16, reason: &str) -> Vec<u8> {
    let reason = reason.as_bytes();
    let reason_len = reason.len().min(1024) as u16;
    let mut payload = Vec::with_capacity(8 + reason_len as usize);
    payload.extend_from_slice(&acknowledged_sequence.to_be_bytes());
    payload.extend_from_slice(&reason_code.to_be_bytes());
    payload.extend_from_slice(&reason_len.to_be_bytes());
    payload.extend_from_slice(&reason[..reason_len as usize]);
    payload
}

pub struct InputServer {
    listener: TcpListener,
    active: Arc<AtomicBool>,
}

impl InputServer {
    pub fn bind(bind: &str) -> Result<Self> {
        let listener = TcpListener::bind(bind)
            .with_context(|| format!("failed to bind pointer-control listener at {bind}"))?;
        Ok(Self {
            listener,
            active: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener.local_addr().context("query local addr")
    }

    pub fn start_logging(self) -> thread::JoinHandle<Result<()>> {
        thread::Builder::new()
            .name("window-warp-input-server".into())
            .spawn(move || loop {
                let (mut stream, peer) = self.listener.accept().context("accept control client")?;
                if self
                    .active
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    write_reason_message(
                        &mut stream,
                        MessageType::Reject,
                        0,
                        REASON_BUSY,
                        "busy",
                    )?;
                    continue;
                }

                println!("accepted sidecar control client from {peer}");
                let mut sink = SystemPointerSink::default();
                let result = handle_client_with_sink(&mut stream, &mut sink);
                self.active.store(false, Ordering::SeqCst);
                result?;
            })
            .expect("spawn pointer-control listener thread")
    }
}

#[derive(Default)]
struct ControlSession {
    last_sequence: u32,
}

fn handle_client_with_sink(stream: &mut TcpStream, sink: &mut dyn PointerSink) -> Result<()> {
    let message = read_message(stream)?;
    if message.kind != MessageType::Hello {
        write_reason_message(
            stream,
            MessageType::Reject,
            message.sequence,
            REASON_INVALID_MESSAGE,
            "control client must begin with Hello",
        )?;
        anyhow::bail!("control client must begin with Hello");
    }
    let hello = decode_hello(&message.payload)?;
    println!(
        "sidecar control hello width={} height={} pointer_flags=0x{:x}",
        hello.display_width, hello.display_height, hello.pointer_flags
    );
    write_message(
        stream,
        &Message {
            kind: MessageType::HelloAck,
            sequence: message.sequence,
            reserved: 0,
            payload: encode_hello(Hello {
                display_width: hello.display_width,
                display_height: hello.display_height,
                pointer_flags: POINTER_CAPABILITY_FLAG,
            }),
        },
    )?;

    let mut session = ControlSession {
        last_sequence: message.sequence,
    };
    loop {
        let message = match read_message(stream) {
            Ok(message) => message,
            Err(error) if error.to_string().contains("failed to fill whole buffer") => return Ok(()),
            Err(error) => return Err(error),
        };
        if !process_message(stream, sink, &mut session, message)? {
            return Ok(());
        }
    }
}

fn process_message(
    stream: &mut TcpStream,
    sink: &mut dyn PointerSink,
    session: &mut ControlSession,
    message: Message,
) -> Result<bool> {
    match message.kind {
        MessageType::Heartbeat => {
            write_message(
                stream,
                &Message {
                    kind: MessageType::Heartbeat,
                    sequence: message.sequence,
                    reserved: 0,
                    payload: Vec::new(),
                },
            )?;
            Ok(true)
        }
        MessageType::PointerMove => {
            if message.sequence <= session.last_sequence {
                write_reason_message(
                    stream,
                    MessageType::Outcome,
                    message.sequence,
                    REASON_OUT_OF_ORDER,
                    "pointer sequence out of order",
                )?;
                return Ok(true);
            }
            let point = match decode_pointer_move(&message.payload) {
                Ok(point) => point,
                Err(_) => {
                    write_reason_message(
                        stream,
                        MessageType::Outcome,
                        message.sequence,
                        REASON_INVALID_MESSAGE,
                        "invalid pointer move payload",
                    )?;
                    return Ok(true);
                }
            };
            sink.move_pointer(point)?;
            session.last_sequence = message.sequence;
            write_reason_message(stream, MessageType::Outcome, message.sequence, REASON_OK, "")?;
            Ok(true)
        }
        MessageType::PointerButton => {
            if message.sequence <= session.last_sequence {
                write_reason_message(
                    stream,
                    MessageType::Outcome,
                    message.sequence,
                    REASON_OUT_OF_ORDER,
                    "pointer sequence out of order",
                )?;
                return Ok(true);
            }
            let action = match decode_pointer_button(&message.payload) {
                Ok(action) => action,
                Err(error) => {
                    let code = if error.to_string().contains("unsupported pointer button") {
                        REASON_UNSUPPORTED_BUTTON
                    } else {
                        REASON_INVALID_MESSAGE
                    };
                    write_reason_message(
                        stream,
                        MessageType::Outcome,
                        message.sequence,
                        code,
                        &error.to_string(),
                    )?;
                    return Ok(true);
                }
            };
            sink.button_pointer(action)?;
            session.last_sequence = message.sequence;
            write_reason_message(stream, MessageType::Outcome, message.sequence, REASON_OK, "")?;
            Ok(true)
        }
        MessageType::Disconnect => Ok(false),
        other => {
            write_reason_message(
                stream,
                MessageType::Outcome,
                message.sequence,
                REASON_INVALID_MESSAGE,
                &format!("unexpected control message {:?}", other),
            )?;
            Ok(true)
        }
    }
}

fn read_message(stream: &mut TcpStream) -> Result<Message> {
    let mut header = [0u8; HEADER_SIZE];
    stream.read_exact(&mut header).context("read control header")?;
    if header[0..2] != MAGIC {
        anyhow::bail!("invalid control magic");
    }
    let payload_len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        anyhow::bail!("oversized control payload {payload_len}");
    }
    let mut payload = vec![0u8; payload_len];
    stream
        .read_exact(&mut payload)
        .context("read control payload")?;
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&payload);
    decode(&bytes)
}

fn write_message(stream: &mut TcpStream, message: &Message) -> Result<()> {
    stream
        .write_all(&encode(message))
        .context("write control response")
}

fn write_reason_message(
    stream: &mut TcpStream,
    kind: MessageType,
    acknowledged_sequence: u32,
    reason_code: u16,
    reason: &str,
) -> Result<()> {
    write_message(
        stream,
        &Message {
            kind,
            sequence: acknowledged_sequence,
            reserved: 0,
            payload: encode_outcome(acknowledged_sequence, reason_code, reason),
        },
    )
}

#[cfg(windows)]
#[derive(Default)]
struct SystemPointerSink;

#[cfg(windows)]
impl SystemPointerSink {
    fn set_position(point: NormalizedPoint) -> Result<()> {
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1) - 1;
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1) - 1;
        let x = ((point.x as u32 * width as u32) / u16::MAX as u32) as i32;
        let y = ((point.y as u32 * height as u32) / u16::MAX as u32) as i32;
        unsafe { SetCursorPos(x, y) }.ok().context("move system pointer")
    }

    fn send_mouse(flag: u32) -> Result<()> {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSE_EVENT_FLAGS(flag),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(anyhow!("SendInput reported {sent} events"))
        }
    }
}

#[cfg(windows)]
impl PointerSink for SystemPointerSink {
    fn move_pointer(&mut self, point: NormalizedPoint) -> Result<()> {
        Self::set_position(point)
    }

    fn button_pointer(&mut self, action: PointerButtonAction) -> Result<()> {
        Self::set_position(action.point)?;
        let flag = match (action.button, action.pressed) {
            (PointerButton::Primary, true) => MOUSEEVENTF_LEFTDOWN.0,
            (PointerButton::Primary, false) => MOUSEEVENTF_LEFTUP.0,
            (PointerButton::Secondary, true) => MOUSEEVENTF_RIGHTDOWN.0,
            (PointerButton::Secondary, false) => MOUSEEVENTF_RIGHTUP.0,
            (PointerButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN.0,
            (PointerButton::Middle, false) => MOUSEEVENTF_MIDDLEUP.0,
            (PointerButton::None, _) => return Ok(()),
        };
        Self::send_mouse(flag)
    }
}

#[cfg(not(windows))]
#[derive(Default)]
struct SystemPointerSink;

#[cfg(not(windows))]
impl PointerSink for SystemPointerSink {
    fn move_pointer(&mut self, _point: NormalizedPoint) -> Result<()> {
        Ok(())
    }

    fn button_pointer(&mut self, _action: PointerButtonAction) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingPointerSink {
        moves: Vec<NormalizedPoint>,
        buttons: Vec<PointerButtonAction>,
    }

    impl PointerSink for RecordingPointerSink {
        fn move_pointer(&mut self, point: NormalizedPoint) -> Result<()> {
            self.moves.push(point);
            Ok(())
        }

        fn button_pointer(&mut self, action: PointerButtonAction) -> Result<()> {
            self.buttons.push(action);
            Ok(())
        }
    }

    fn connect(server: &InputServer) -> TcpStream {
        TcpStream::connect(server.local_addr().unwrap()).unwrap()
    }

    #[test]
    fn encodes_and_decodes_messages() {
        let hello = Hello {
            display_width: 800,
            display_height: 480,
            pointer_flags: 3,
        };
        let message = Message {
            kind: MessageType::Hello,
            sequence: 9,
            reserved: 0,
            payload: encode_hello(hello),
        };
        let decoded = decode(&encode(&message)).unwrap();
        assert_eq!(decoded.kind, MessageType::Hello);
        assert_eq!(decoded.sequence, 9);
        assert_eq!(decode_hello(&decoded.payload).unwrap(), hello);
    }

    #[test]
    fn encodes_pointer_payloads() {
        let point = NormalizedPoint { x: 1024, y: 2048 };
        assert_eq!(decode_pointer_move(&encode_pointer_move(point)).unwrap(), point);
        let action = PointerButtonAction {
            point,
            button: PointerButton::Primary,
            pressed: true,
        };
        assert_eq!(decode_pointer_button(&encode_pointer_button(action)).unwrap(), action);
    }

    #[test]
    fn enforces_single_active_session() {
        let server = InputServer::bind("127.0.0.1:0").unwrap();
        assert!(
            server
                .active
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        );
        assert!(
            server
                .active
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        );
    }

    #[test]
    fn accepts_hello_and_replies_with_hello_ack_and_heartbeat() {
        let server = InputServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = server.listener.accept().unwrap();
            let mut sink = RecordingPointerSink::default();
            handle_client_with_sink(&mut stream, &mut sink).unwrap();
        });

        let mut client = TcpStream::connect(addr).unwrap();
        write_message(
            &mut client,
            &Message {
                kind: MessageType::Hello,
                sequence: 5,
                reserved: 0,
                payload: encode_hello(Hello {
                    display_width: 640,
                    display_height: 480,
                    pointer_flags: POINTER_CAPABILITY_FLAG,
                }),
            },
        )
        .unwrap();
        let reply = read_message(&mut client).unwrap();
        assert_eq!(reply.kind, MessageType::HelloAck);
        assert_eq!(reply.sequence, 5);
        let reply_hello = decode_hello(&reply.payload).unwrap();
        assert_eq!(reply_hello.pointer_flags, POINTER_CAPABILITY_FLAG);

        write_message(
            &mut client,
            &Message {
                kind: MessageType::Heartbeat,
                sequence: 6,
                reserved: 0,
                payload: Vec::new(),
            },
        )
        .unwrap();
        let heartbeat = read_message(&mut client).unwrap();
        assert_eq!(heartbeat.kind, MessageType::Heartbeat);
        assert_eq!(heartbeat.sequence, 6);
        write_message(
            &mut client,
            &Message {
                kind: MessageType::Disconnect,
                sequence: 7,
                reserved: 0,
                payload: Vec::new(),
            },
        )
        .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn applies_pointer_messages_in_order() {
        let server = InputServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = server.listener.accept().unwrap();
            let mut sink = RecordingPointerSink::default();
            handle_client_with_sink(&mut stream, &mut sink).unwrap();
            sink
        });

        let mut client = TcpStream::connect(addr).unwrap();
        write_message(
            &mut client,
            &Message {
                kind: MessageType::Hello,
                sequence: 1,
                reserved: 0,
                payload: encode_hello(Hello {
                    display_width: 720,
                    display_height: 720,
                    pointer_flags: POINTER_CAPABILITY_FLAG,
                }),
            },
        )
        .unwrap();
        let _ = read_message(&mut client).unwrap();

        write_message(
            &mut client,
            &Message {
                kind: MessageType::PointerMove,
                sequence: 2,
                reserved: 0,
                payload: encode_pointer_move(NormalizedPoint { x: 100, y: 200 }),
            },
        )
        .unwrap();
        let outcome = read_message(&mut client).unwrap();
        assert_eq!(outcome.kind, MessageType::Outcome);

        write_message(
            &mut client,
            &Message {
                kind: MessageType::PointerButton,
                sequence: 3,
                reserved: 0,
                payload: encode_pointer_button(PointerButtonAction {
                    point: NormalizedPoint { x: 100, y: 200 },
                    button: PointerButton::Primary,
                    pressed: true,
                }),
            },
        )
        .unwrap();
        let outcome = read_message(&mut client).unwrap();
        assert_eq!(outcome.kind, MessageType::Outcome);

        write_message(
            &mut client,
            &Message {
                kind: MessageType::Disconnect,
                sequence: 4,
                reserved: 0,
                payload: Vec::new(),
            },
        )
        .unwrap();

        let sink = handle.join().unwrap();
        assert_eq!(sink.moves, vec![NormalizedPoint { x: 100, y: 200 }]);
        assert_eq!(
            sink.buttons,
            vec![PointerButtonAction {
                point: NormalizedPoint { x: 100, y: 200 },
                button: PointerButton::Primary,
                pressed: true,
            }]
        );
    }

    #[test]
    fn rejects_out_of_order_pointer_messages() {
        let server = InputServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = server.listener.accept().unwrap();
            let mut sink = RecordingPointerSink::default();
            handle_client_with_sink(&mut stream, &mut sink).unwrap();
        });

        let mut client = TcpStream::connect(addr).unwrap();
        write_message(
            &mut client,
            &Message {
                kind: MessageType::Hello,
                sequence: 10,
                reserved: 0,
                payload: encode_hello(Hello {
                    display_width: 720,
                    display_height: 720,
                    pointer_flags: POINTER_CAPABILITY_FLAG,
                }),
            },
        )
        .unwrap();
        let _ = read_message(&mut client).unwrap();

        write_message(
            &mut client,
            &Message {
                kind: MessageType::PointerMove,
                sequence: 9,
                reserved: 0,
                payload: encode_pointer_move(NormalizedPoint { x: 1, y: 2 }),
            },
        )
        .unwrap();
        let outcome = read_message(&mut client).unwrap();
        assert_eq!(outcome.kind, MessageType::Outcome);
        let payload = outcome.payload;
        assert_eq!(u16::from_be_bytes(payload[4..6].try_into().unwrap()), REASON_OUT_OF_ORDER);

        write_message(
            &mut client,
            &Message {
                kind: MessageType::Disconnect,
                sequence: 11,
                reserved: 0,
                payload: Vec::new(),
            },
        )
        .unwrap();
        handle.join().unwrap();
    }
}
