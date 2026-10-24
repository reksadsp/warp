use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use anyhow::{Context, Result, anyhow};
use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};

const MAGIC: [u8; 2] = *b"WS";
const VERSION: u8 = 1;
const HEADER_SIZE: usize = 16;
const HELLO: u8 = 1;
const HELLO_ACK: u8 = 2;
const POINTER_MOVE: u8 = 5;
const POINTER_BUTTON: u8 = 6;
const OUTCOME: u8 = 7;
const DISCONNECT: u8 = 8;
const POINTER_CAPABILITY: u32 = 1;

pub fn start(bind: &str, source_size: (u32, u32), origin: (i32, i32)) -> Result<()> {
    let listener = TcpListener::bind(bind)
        .with_context(|| format!("bind remote pointer-control listener at {bind}"))?;
    let address = listener
        .local_addr()
        .context("query pointer-control listener")?;
    println!("trusted-LAN pointer control listening on tcp://{address}");
    thread::Builder::new()
        .name("unix-warp-input-server".into())
        .spawn(move || -> Result<()> {
            loop {
                let (stream, peer) = listener.accept().context("accept pointer-control client")?;
                println!("accepted pointer-control client from {peer}");
                if let Err(error) = handle_client(stream, source_size, origin) {
                    eprintln!("pointer-control client ended: {error:#}");
                }
            }
        })
        .context("start pointer-control listener")?;
    Ok(())
}

fn handle_client(mut stream: TcpStream, source_size: (u32, u32), origin: (i32, i32)) -> Result<()> {
    let (kind, sequence, hello) = read_message(&mut stream)?;
    if kind != HELLO || hello.len() != 8 {
        anyhow::bail!("pointer-control client must begin with a valid hello");
    }
    let mut ack = hello[..4].to_vec();
    ack.extend_from_slice(&POINTER_CAPABILITY.to_be_bytes());
    write_message(&mut stream, HELLO_ACK, sequence, &ack)?;

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|error| anyhow!("initialize native pointer injection: {error}"))?;
    let mut last_sequence = sequence;
    loop {
        let (kind, sequence, payload) = read_message(&mut stream)?;
        if kind == DISCONNECT {
            return Ok(());
        }
        if sequence <= last_sequence {
            write_outcome(&mut stream, sequence, 4, "pointer sequence out of order")?;
            continue;
        }
        let result = match kind {
            POINTER_MOVE if payload.len() == 4 => {
                move_pointer(&mut enigo, &payload, source_size, origin)
            }
            POINTER_BUTTON if payload.len() == 6 => {
                move_pointer(&mut enigo, &payload[..4], source_size, origin)
                    .and_then(|()| button_pointer(&mut enigo, &payload[4..]))
            }
            _ => Err(anyhow!("invalid pointer-control message")),
        };
        match result {
            Ok(()) => {
                last_sequence = sequence;
                write_outcome(&mut stream, sequence, 0, "")?;
            }
            Err(error) => write_outcome(&mut stream, sequence, 2, &error.to_string())?,
        }
    }
}

fn move_pointer(
    enigo: &mut Enigo,
    payload: &[u8],
    source_size: (u32, u32),
    origin: (i32, i32),
) -> Result<()> {
    let x = u16::from_be_bytes(payload[..2].try_into().unwrap()) as u32;
    let y = u16::from_be_bytes(payload[2..4].try_into().unwrap()) as u32;
    let x = origin.0 + (x * source_size.0.saturating_sub(1) / u16::MAX as u32) as i32;
    let y = origin.1 + (y * source_size.1.saturating_sub(1) / u16::MAX as u32) as i32;
    enigo
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|error| anyhow!("transfer pointer motion: {error}"))
}

fn button_pointer(enigo: &mut Enigo, payload: &[u8]) -> Result<()> {
    let button = match payload[0] {
        1 => Button::Left,
        2 => Button::Right,
        3 => Button::Middle,
        _ => anyhow::bail!("unsupported pointer button"),
    };
    let direction = match payload[1] {
        0 => Direction::Release,
        1 => Direction::Press,
        _ => anyhow::bail!("invalid pointer button phase"),
    };
    enigo
        .button(button, direction)
        .map_err(|error| anyhow!("transfer pointer button: {error}"))
}

fn read_message(stream: &mut TcpStream) -> Result<(u8, u32, Vec<u8>)> {
    let mut header = [0; HEADER_SIZE];
    stream
        .read_exact(&mut header)
        .context("read pointer-control header")?;
    if header[..2] != MAGIC || header[2] != VERSION {
        anyhow::bail!("invalid pointer-control protocol header");
    }
    let payload_len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    if payload_len > 4096 {
        anyhow::bail!("oversized pointer-control payload");
    }
    let mut payload = vec![0; payload_len];
    stream
        .read_exact(&mut payload)
        .context("read pointer-control payload")?;
    Ok((
        header[3],
        u32::from_be_bytes(header[8..12].try_into().unwrap()),
        payload,
    ))
}

fn write_message(stream: &mut TcpStream, kind: u8, sequence: u32, payload: &[u8]) -> Result<()> {
    let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len());
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&[VERSION, kind]);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(payload);
    stream
        .write_all(&bytes)
        .context("write pointer-control message")
}

fn write_outcome(stream: &mut TcpStream, sequence: u32, reason: u16, detail: &str) -> Result<()> {
    let detail = detail.as_bytes();
    let mut payload = Vec::with_capacity(8 + detail.len());
    payload.extend_from_slice(&sequence.to_be_bytes());
    payload.extend_from_slice(&reason.to_be_bytes());
    payload.extend_from_slice(&(detail.len().min(1024) as u16).to_be_bytes());
    payload.extend_from_slice(&detail[..detail.len().min(1024)]);
    write_message(stream, OUTCOME, sequence, &payload)
}
