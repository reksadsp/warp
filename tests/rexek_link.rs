use rexek_link::{
    CAP_CAPTURE, CAP_STATE, Client, Connection, Error, Frame, HEADER_SIZE, Identity, MAX_CONTROL,
    REJECT_ALREADY_CONNECTED, Type,
    app_state::{AppState, Cli, EVENT_LOG_LIMIT, RETRY_DELAYS, SessionUi},
    decode, decode_hello_ack, decode_reject, decode_snapshot, encode, encode_hello,
    encode_hello_ack,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

// Golden `Hello` frame for host "warp", capabilities state|capture, correlation 7.
// The embedded device test suite asserts the identical byte sequence.
const HELLO_WIRE: [u8; 26] = [
    82, 88, 1, 1, 0, 0, 0, 16, 0, 0, 0, 10, 0, 0, 0, 7, 0, 4, b'w', b'a', b'r', b'p', 0, 0, 0, 3,
];

#[test]
fn codec_round_trip_and_malformed_input() {
    let frame = Frame {
        kind: Type::Hello,
        correlation_id: 9,
        payload: vec![1, 0, 0, 0, 3, b'w'],
    };
    assert_eq!(decode(&encode(&frame).unwrap()).unwrap(), frame);
    assert_eq!(decode(&[0; HEADER_SIZE]), Err(Error::Magic));
    let mut version = encode(&frame).unwrap();
    version[2] = 2;
    assert_eq!(decode(&version), Err(Error::Version));
    let mut flags = encode(&frame).unwrap();
    flags[4] = 1;
    assert_eq!(decode(&flags), Err(Error::Flags));
    let mut header_size = encode(&frame).unwrap();
    header_size[6] = 1;
    assert_eq!(decode(&header_size), Err(Error::HeaderSize));
    let mut truncated = encode(&frame).unwrap();
    truncated.pop();
    assert_eq!(decode(&truncated), Err(Error::Truncated));
    let mut unknown = encode(&frame).unwrap();
    unknown[3] = 42;
    assert_eq!(decode(&unknown), Err(Error::UnknownType));
    assert_eq!(
        encode(&Frame {
            kind: Type::Hello,
            correlation_id: 0,
            payload: vec![0; MAX_CONTROL + 1]
        }),
        Err(Error::Oversized)
    );
}

#[test]
fn capture_limits_are_bounded() {
    let frame = Frame {
        kind: Type::CaptureChunk,
        correlation_id: 1,
        payload: vec![0; 60 * 1024 + 1],
    };
    assert_eq!(encode(&frame), Err(Error::Oversized));
}

#[test]
fn hello_payload_matches_device_wire_format() {
    let payload = encode_hello("warp", CAP_STATE | CAP_CAPTURE).unwrap();
    let frame = Frame {
        kind: Type::Hello,
        correlation_id: 7,
        payload,
    };
    assert_eq!(encode(&frame).unwrap(), HELLO_WIRE.to_vec());
    assert_eq!(encode_hello("", CAP_STATE), Err(Error::RequiredField));
    assert_eq!(
        encode_hello(&"x".repeat(129), CAP_STATE),
        Err(Error::RequiredField)
    );
}

#[test]
fn hello_ack_identity_is_validated() {
    let identity = Identity {
        session_id: 5,
        device_id: "rexek".into(),
        software_version: "1.1.1".into(),
        capabilities: CAP_STATE | CAP_CAPTURE,
    };
    let payload = encode_hello_ack(&identity).unwrap();
    assert_eq!(decode_hello_ack(&payload).unwrap(), identity);
    // A zero session ID or missing fields must be rejected.
    let mut malformed = payload.clone();
    malformed[0] = 0;
    malformed[1] = 0;
    malformed[2] = 0;
    malformed[3] = 0;
    assert_eq!(decode_hello_ack(&malformed), Err(Error::RequiredField));
    assert_eq!(
        decode_hello_ack(&payload[..payload.len() - 4]),
        Err(Error::RequiredField)
    );
    let mut empty_id = payload.clone();
    empty_id[4] = 0;
    empty_id[5] = 0;
    assert_eq!(decode_hello_ack(&empty_id), Err(Error::RequiredField));
}

#[test]
fn snapshot_and_reject_payloads_decode() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1234u64.to_be_bytes());
    payload.push(2);
    payload.extend_from_slice(&3u32.to_be_bytes());
    payload.extend_from_slice(&5u16.to_be_bytes());
    payload.extend_from_slice(b"rexek");
    let snapshot = decode_snapshot(&payload).unwrap();
    assert_eq!(snapshot.timestamp_ms, 1234);
    assert_eq!(snapshot.connection, Connection::Active);
    assert_eq!(snapshot.capabilities, 3);
    assert_eq!(snapshot.device_id, "rexek");

    assert_eq!(
        decode_reject(&REJECT_ALREADY_CONNECTED.to_be_bytes()).unwrap(),
        REJECT_ALREADY_CONNECTED
    );
    assert_eq!(decode_reject(&[0, 0]), Err(Error::RequiredField));
}

// Reads one complete frame from the stream, returning kind and correlation.
fn read_frame(stream: &mut TcpStream) -> Frame {
    let mut header = [0; HEADER_SIZE];
    stream.read_exact(&mut header).unwrap();
    let size = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
    let mut payload = vec![0; size];
    stream.read_exact(&mut payload).unwrap();
    decode(&[header.as_slice(), payload.as_slice()].concat()).unwrap()
}

// Spawns a mock device on loopback that answers one host connection.
fn mock_device(respond: impl FnOnce(&mut TcpStream) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        respond(&mut stream);
    });
    address
}

#[test]
fn handshake_reaches_active_session() {
    let address = mock_device(|stream| {
        let hello = read_frame(stream);
        assert_eq!(hello.kind, Type::Hello);
        assert_eq!(hello.payload, HELLO_WIRE[HEADER_SIZE..].to_vec());
        let identity = Identity {
            session_id: 5,
            device_id: "rexek".into(),
            software_version: "1.1.1".into(),
            capabilities: CAP_STATE | CAP_CAPTURE,
        };
        let ack = Frame {
            kind: Type::HelloAck,
            correlation_id: hello.correlation_id,
            payload: encode_hello_ack(&identity).unwrap(),
        };
        stream.write_all(&encode(&ack).unwrap()).unwrap();
        // Hold the session open until the client closes the connection.
        let mut sink = [0; 64];
        while matches!(stream.read(&mut sink), Ok(count) if count > 0) {}
    });
    let mut client = Client::new();
    client
        .connect(&address, "warp", CAP_STATE | CAP_CAPTURE)
        .unwrap();
    for _ in 0..50 {
        if client.state == Connection::Active {
            break;
        }
        client.poll().unwrap();
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(client.state, Connection::Active);
    let identity = client.identity.as_ref().unwrap();
    assert_eq!(identity.session_id, 5);
    assert_eq!(identity.device_id, "rexek");
    assert_eq!(identity.software_version, "1.1.1");
    assert_eq!(identity.capabilities, CAP_STATE | CAP_CAPTURE);
    assert!(client.last_reject.is_none());
    client.disconnect();
}

#[test]
fn rejection_disconnects_without_session() {
    let address = mock_device(|stream| {
        let hello = read_frame(stream);
        let reject = Frame {
            kind: Type::Reject,
            correlation_id: hello.correlation_id,
            payload: REJECT_ALREADY_CONNECTED.to_be_bytes().to_vec(),
        };
        stream.write_all(&encode(&reject).unwrap()).unwrap();
    });
    let mut client = Client::new();
    client
        .connect(&address, "warp", CAP_STATE | CAP_CAPTURE)
        .unwrap();
    for _ in 0..50 {
        if client.state == Connection::Disconnected && client.identity.is_none() {
            break;
        }
        client.poll().unwrap();
        thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(client.state, Connection::Disconnected);
    assert_eq!(client.last_reject, Some(REJECT_ALREADY_CONNECTED));
    assert!(client.identity.is_none());
}

#[test]
fn cli_defaults_and_capture_are_explicit() {
    assert_eq!(Cli::parse(Vec::<String>::new()).unwrap(), Cli::default());
    assert_eq!(
        Cli::parse(
            [
                "--device",
                "192.0.2.8:47052",
                "--host-id",
                "operator",
                "--capture-screen"
            ]
            .into_iter()
            .map(String::from)
        )
        .unwrap(),
        Cli {
            device: "192.0.2.8:47052".into(),
            host_id: "operator".into(),
            capture_screen: true
        }
    );
    assert!(Cli::parse(["--device"].into_iter().map(String::from)).is_err());
    assert!(Cli::parse(["--unknown"].into_iter().map(String::from)).is_err());
}

#[test]
fn event_log_is_bounded_and_retry_is_capped() {
    let mut state = AppState::default();
    for index in 0..EVENT_LOG_LIMIT + 5 {
        state.record(format!("event-{index}"));
    }
    assert_eq!(state.events.len(), EVENT_LOG_LIMIT);
    assert_eq!(state.events.front().unwrap(), "event-5");

    let observed: Vec<_> = (0..8).map(|_| state.next_retry_delay()).collect();
    assert_eq!(&observed[..RETRY_DELAYS.len()], &RETRY_DELAYS);
    assert_eq!(observed[5], *RETRY_DELAYS.last().unwrap());
    state.reject(REJECT_ALREADY_CONNECTED);
    assert_eq!(state.session, SessionUi::Rejected(REJECT_ALREADY_CONNECTED));
}
