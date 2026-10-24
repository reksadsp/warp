use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result};
use enigo::{Button, Direction};

use crate::pointer::SourcePoint;

const MAGIC: [u8; 2] = *b"WS";
const VERSION: u8 = 1;
const HEADER_SIZE: usize = 16;
const POINTER_CAPABILITY: u32 = 1;
const HELLO: u8 = 1;
const HELLO_ACK: u8 = 2;
const POINTER_MOVE: u8 = 5;
const POINTER_BUTTON: u8 = 6;
const OUTCOME: u8 = 7;

pub struct RemotePointer {
    stream: TcpStream,
    source_size: (u32, u32),
    next_sequence: u32,
}

impl RemotePointer {
    pub fn connect(address: &str, source_size: (u32, u32)) -> Result<Self> {
        let stream = TcpStream::connect(address)
            .with_context(|| format!("connect to remote pointer server at {address}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .context("set pointer server read timeout")?;
        let mut pointer = Self {
            stream,
            source_size,
            next_sequence: 1,
        };
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&(source_size.0.min(u16::MAX as u32) as u16).to_be_bytes());
        payload.extend_from_slice(&(source_size.1.min(u16::MAX as u32) as u16).to_be_bytes());
        payload.extend_from_slice(&POINTER_CAPABILITY.to_be_bytes());
        pointer.send(HELLO, &payload)?;
        let (kind, sequence, payload) = pointer.read()?;
        if kind != HELLO_ACK || sequence != 1 || payload.len() != 8 {
            anyhow::bail!("remote pointer server rejected the handshake");
        }
        if u32::from_be_bytes(payload[4..8].try_into().unwrap()) & POINTER_CAPABILITY == 0 {
            anyhow::bail!("remote pointer server does not support pointer input");
        }
        Ok(pointer)
    }

    pub fn move_to(&mut self, point: SourcePoint) -> Result<()> {
        self.send(POINTER_MOVE, &encode_point(point, self.source_size))?;
        self.expect_outcome()
    }

    pub fn button(
        &mut self,
        point: SourcePoint,
        button: Button,
        direction: Direction,
    ) -> Result<()> {
        let button = match button {
            Button::Left => 1,
            Button::Right => 2,
            Button::Middle => 3,
            _ => anyhow::bail!("remote pointer protocol does not support this mouse button"),
        };
        let pressed = match direction {
            Direction::Press => 1,
            Direction::Release => 0,
            _ => anyhow::bail!("remote pointer protocol does not support this button direction"),
        };
        let mut payload = encode_point(point, self.source_size);
        payload.extend_from_slice(&[button, pressed]);
        self.send(POINTER_BUTTON, &payload)?;
        self.expect_outcome()
    }

    fn send(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&[VERSION, kind]);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(payload);
        self.stream
            .write_all(&bytes)
            .context("send remote pointer event")
    }

    fn read(&mut self) -> Result<(u8, u32, Vec<u8>)> {
        let mut header = [0; HEADER_SIZE];
        self.stream
            .read_exact(&mut header)
            .context("read remote pointer header")?;
        if header[..2] != MAGIC || header[2] != VERSION {
            anyhow::bail!("invalid remote pointer response");
        }
        let payload_len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
        if payload_len > 4096 {
            anyhow::bail!("remote pointer response is oversized");
        }
        let mut payload = vec![0; payload_len];
        self.stream
            .read_exact(&mut payload)
            .context("read remote pointer payload")?;
        Ok((
            header[3],
            u32::from_be_bytes(header[8..12].try_into().unwrap()),
            payload,
        ))
    }

    fn expect_outcome(&mut self) -> Result<()> {
        let expected = self.next_sequence.wrapping_sub(1).max(1);
        let (kind, sequence, payload) = self.read()?;
        if kind != OUTCOME || sequence != expected || payload.len() < 8 {
            anyhow::bail!("remote pointer server returned an invalid outcome");
        }
        if u32::from_be_bytes(payload[..4].try_into().unwrap()) != expected {
            anyhow::bail!("remote pointer outcome acknowledged a different event");
        }
        let reason = u16::from_be_bytes(payload[4..6].try_into().unwrap());
        if reason != 0 {
            anyhow::bail!(
                "remote pointer event rejected ({reason}): {}",
                String::from_utf8_lossy(&payload[8..])
            );
        }
        Ok(())
    }
}

fn encode_point(point: SourcePoint, source_size: (u32, u32)) -> Vec<u8> {
    let x = point.x.clamp(0, source_size.0.saturating_sub(1) as i32) as u32;
    let y = point.y.clamp(0, source_size.1.saturating_sub(1) as i32) as u32;
    let x = (x * u16::MAX as u32 / source_size.0.saturating_sub(1).max(1)) as u16;
    let y = (y * u16::MAX as u32 / source_size.1.saturating_sub(1).max(1)) as u16;
    [x.to_be_bytes(), y.to_be_bytes()].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn read_wire(stream: &mut TcpStream) -> (u8, u32, Vec<u8>) {
        let mut header = [0; HEADER_SIZE];
        stream.read_exact(&mut header).unwrap();
        let mut payload = vec![0; u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize];
        stream.read_exact(&mut payload).unwrap();
        (
            header[3],
            u32::from_be_bytes(header[8..12].try_into().unwrap()),
            payload,
        )
    }

    fn write_wire(stream: &mut TcpStream, kind: u8, sequence: u32, payload: &[u8]) {
        let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&[VERSION, kind]);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(payload);
        stream.write_all(&bytes).unwrap();
    }

    fn write_outcome(stream: &mut TcpStream, sequence: u32) {
        let mut payload = Vec::new();
        payload.extend_from_slice(&sequence.to_be_bytes());
        payload.extend_from_slice(&0u16.to_be_bytes());
        payload.extend_from_slice(&0u16.to_be_bytes());
        write_wire(stream, OUTCOME, sequence, &payload);
    }

    #[test]
    fn normalizes_source_points() {
        assert_eq!(
            encode_point(SourcePoint { x: 0, y: 1079 }, (1920, 1080)),
            [0, 0, 255, 255]
        );
    }

    #[test]
    fn sends_normalized_pointer_events_after_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (kind, sequence, hello) = read_wire(&mut stream);
            assert_eq!((kind, sequence), (HELLO, 1));
            assert_eq!(hello, [7, 128, 4, 56, 0, 0, 0, 1]);

            let mut ack = Vec::new();
            ack.extend_from_slice(&1920u16.to_be_bytes());
            ack.extend_from_slice(&1080u16.to_be_bytes());
            ack.extend_from_slice(&POINTER_CAPABILITY.to_be_bytes());
            write_wire(&mut stream, HELLO_ACK, 1, &ack);

            let (kind, sequence, payload) = read_wire(&mut stream);
            assert_eq!((kind, sequence), (POINTER_MOVE, 2));
            assert_eq!(payload, [255, 255, 255, 255]);
            write_outcome(&mut stream, sequence);

            let (kind, sequence, payload) = read_wire(&mut stream);
            assert_eq!((kind, sequence), (POINTER_BUTTON, 3));
            assert_eq!(payload, [255, 255, 255, 255, 1, 1]);
            write_outcome(&mut stream, sequence);
        });

        let mut pointer = RemotePointer::connect(&address.to_string(), (1920, 1080)).unwrap();
        pointer.move_to(SourcePoint { x: 1919, y: 1079 }).unwrap();
        pointer
            .button(
                SourcePoint { x: 1919, y: 1079 },
                Button::Left,
                Direction::Press,
            )
            .unwrap();
        server.join().unwrap();
    }
}
