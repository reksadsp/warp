use std::collections::HashSet;
use std::env;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

const CONTROL_MAGIC: [u8; 2] = *b"WS";
const CONTROL_VERSION: u8 = 1;
const CONTROL_HEADER_SIZE: usize = 16;
const POINTER_CAPABILITY_FLAG: u32 = 1;
const REASON_OK: u16 = 0;

const HELLO: u8 = 1;
const HELLO_ACK: u8 = 2;
const POINTER_MOVE: u8 = 5;
const POINTER_BUTTON: u8 = 6;
const OUTCOME: u8 = 7;
const DISCONNECT: u8 = 8;

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

fn parse_pointer(value: &str) -> (u16, u16) {
    let (x, y) = value
        .split_once(',')
        .unwrap_or_else(|| panic!("WARP_E2E_POINTER must be X,Y, got {value:?}"));
    (
        x.trim()
            .parse()
            .unwrap_or_else(|_| panic!("invalid X in WARP_E2E_POINTER: {x:?}")),
        y.trim()
            .parse()
            .unwrap_or_else(|_| panic!("invalid Y in WARP_E2E_POINTER: {y:?}")),
    )
}

fn write_message(stream: &mut TcpStream, kind: u8, sequence: u32, payload: &[u8]) {
    let mut bytes = Vec::with_capacity(CONTROL_HEADER_SIZE + payload.len());
    bytes.extend_from_slice(&CONTROL_MAGIC);
    bytes.push(CONTROL_VERSION);
    bytes.push(kind);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(payload);
    stream.write_all(&bytes).expect("send control message");
}

fn read_message(stream: &mut TcpStream) -> (u8, u32, Vec<u8>) {
    let mut header = [0u8; CONTROL_HEADER_SIZE];
    stream.read_exact(&mut header).expect("read control header");
    assert_eq!(&header[..2], CONTROL_MAGIC, "invalid control magic");
    assert_eq!(header[2], CONTROL_VERSION, "unsupported control version");
    let payload_len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    assert!(payload_len <= 4096, "oversized control payload");
    let mut payload = vec![0; payload_len];
    stream
        .read_exact(&mut payload)
        .expect("read control payload");
    (
        header[3],
        u32::from_be_bytes(header[8..12].try_into().unwrap()),
        payload,
    )
}

fn assert_ok_outcome(stream: &mut TcpStream, sequence: u32) {
    let (kind, acknowledged_sequence, payload) = read_message(stream);
    assert_eq!(kind, OUTCOME, "control request was rejected");
    assert_eq!(acknowledged_sequence, sequence, "outcome sequence mismatch");
    assert!(payload.len() >= 8, "outcome payload was truncated");
    assert_eq!(
        u32::from_be_bytes(payload[..4].try_into().unwrap()),
        sequence,
        "outcome acknowledged the wrong request"
    );
    assert_eq!(
        u16::from_be_bytes(payload[4..6].try_into().unwrap()),
        REASON_OK,
        "pointer injection failed: {}",
        String::from_utf8_lossy(&payload[8..])
    );
}

fn receive_video(socket: &UdpSocket, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut packet_ids = HashSet::new();
    let mut datagram = [0u8; 1500];

    while Instant::now() < deadline {
        let Ok((length, source)) = socket.recv_from(&mut datagram) else {
            continue;
        };
        if length < 188 || length % 188 != 0 {
            continue;
        }

        let packets = datagram[..length].chunks_exact(188);
        if !packets.clone().all(|packet| packet[0] == 0x47) {
            continue;
        }
        for packet in packets {
            packet_ids.insert(((packet[1] as u16 & 0x1f) << 8) | packet[2] as u16);
        }
        if packet_ids.contains(&0) && packet_ids.contains(&0x100) {
            println!("received MPEG-TS video from {source}");
            return;
        }
    }

    panic!("timed out waiting for MPEG-TS PAT and video PID 0x100");
}

fn connect_control(address: &str, timeout: Duration) -> TcpStream {
    let addresses: Vec<_> = address
        .to_socket_addrs()
        .unwrap_or_else(|error| panic!("resolve pointer-control server {address}: {error}"))
        .collect();
    assert!(
        !addresses.is_empty(),
        "pointer-control server {address} did not resolve"
    );

    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    while Instant::now() < deadline {
        for address in &addresses {
            match TcpStream::connect_timeout(address, Duration::from_secs(1)) {
                Ok(stream) => return stream,
                Err(error) => last_error = Some(error),
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "connect to pointer-control server {address}: {}",
        last_error.expect("control connection attempts were made")
    );
}

#[test]
#[ignore = "requires a running Windows window-warp sender and injects real pointer input"]
fn receives_video_and_sends_pointer_input() {
    assert_eq!(
        required_env("WARP_E2E_ALLOW_POINTER_INJECTION"),
        "1",
        "set WARP_E2E_ALLOW_POINTER_INJECTION=1 to permit the test to move and click the Windows pointer"
    );
    let stream_bind = required_env("WARP_STREAM_BIND");
    let control_address = required_env("WARP_CONTROL_ADDR");
    let point = parse_pointer(&required_env("WARP_E2E_POINTER"));
    let timeout = Duration::from_secs(
        env::var("WARP_E2E_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30),
    );

    let receiver = UdpSocket::bind(&stream_bind)
        .unwrap_or_else(|error| panic!("bind UDP receiver at {stream_bind}: {error}"));
    receiver
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("set video receive timeout");
    println!(
        "UDP receiver ready at {}; start window-warp with --stream udp://<this-host>:{}",
        receiver.local_addr().expect("query UDP receiver address"),
        receiver
            .local_addr()
            .expect("query UDP receiver address")
            .port()
    );
    receive_video(&receiver, timeout);

    let mut control = connect_control(&control_address, timeout);
    control
        .set_read_timeout(Some(timeout))
        .expect("set control read timeout");

    let mut hello = Vec::with_capacity(8);
    hello.extend_from_slice(&1920u16.to_be_bytes());
    hello.extend_from_slice(&1080u16.to_be_bytes());
    hello.extend_from_slice(&POINTER_CAPABILITY_FLAG.to_be_bytes());
    write_message(&mut control, HELLO, 1, &hello);
    let (kind, sequence, payload) = read_message(&mut control);
    assert_eq!(kind, HELLO_ACK, "pointer-control handshake was rejected");
    assert_eq!(sequence, 1, "handshake sequence mismatch");
    assert_eq!(
        payload.len(),
        8,
        "invalid pointer-control hello acknowledgement"
    );
    assert_ne!(
        u32::from_be_bytes(payload[4..8].try_into().unwrap()) & POINTER_CAPABILITY_FLAG,
        0,
        "sender did not advertise pointer-control support"
    );

    let mut move_payload = Vec::with_capacity(4);
    move_payload.extend_from_slice(&point.0.to_be_bytes());
    move_payload.extend_from_slice(&point.1.to_be_bytes());
    write_message(&mut control, POINTER_MOVE, 2, &move_payload);
    assert_ok_outcome(&mut control, 2);

    let mut button_payload = move_payload.clone();
    button_payload.extend_from_slice(&[1, 1]);
    write_message(&mut control, POINTER_BUTTON, 3, &button_payload);
    assert_ok_outcome(&mut control, 3);

    button_payload[5] = 0;
    write_message(&mut control, POINTER_BUTTON, 4, &button_payload);
    assert_ok_outcome(&mut control, 4);
    write_message(&mut control, DISCONNECT, 5, &[]);
}
