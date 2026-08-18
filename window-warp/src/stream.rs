//! UDP sink for the muxed transport stream.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};

use anyhow::{anyhow, Context, Result};

use crate::mpegts::{TsMuxer, TS_PACKET_LEN};

/// Transport packets per datagram: 7 x 188 = 1316 bytes, the usual payload that
/// still fits in an ethernet frame.
const PACKETS_PER_DATAGRAM: usize = 7;
const DATAGRAM_LEN: usize = PACKETS_PER_DATAGRAM * TS_PACKET_LEN;

/// Muxes encoded access units into MPEG-TS and sends them to a UDP address.
pub struct UdpStream {
    socket: UdpSocket,
    destination: SocketAddr,
    muxer: TsMuxer,
    pending: Vec<u8>,
}

impl UdpStream {
    /// `url` is `udp://host:port`, where the host may be a multicast group.
    pub fn open(url: &str, multicast_ttl: u32) -> Result<Self> {
        let destination = parse_udp_url(url)?;
        let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .context("failed to open the UDP socket")?;

        if destination.ip().is_multicast() {
            socket.set_multicast_ttl_v4(multicast_ttl)?;
            socket.set_multicast_loop_v4(true)?;
        } else if destination.ip().is_ipv4() {
            socket.set_broadcast(true).ok();
        }
        socket
            .connect(destination)
            .with_context(|| format!("failed to connect the UDP socket to {destination}"))?;

        Ok(Self {
            socket,
            destination,
            muxer: TsMuxer::new(),
            pending: Vec::with_capacity(DATAGRAM_LEN * 4),
        })
    }

    pub fn destination(&self) -> SocketAddr {
        self.destination
    }

    /// Mux one coded frame and send every datagram it completes.
    pub fn send_access_unit(&mut self, au: &[u8], pts_90khz: u64, keyframe: bool) -> Result<()> {
        self.muxer
            .push_access_unit(au, pts_90khz, keyframe, &mut self.pending);
        self.flush_full_datagrams()
    }

    fn flush_full_datagrams(&mut self) -> Result<()> {
        let mut sent = 0;
        while self.pending.len() - sent >= DATAGRAM_LEN {
            self.socket.send(&self.pending[sent..sent + DATAGRAM_LEN])?;
            sent += DATAGRAM_LEN;
        }
        self.pending.drain(..sent);
        Ok(())
    }

    /// Sends whatever is left, even if it is a short datagram.
    pub fn flush(&mut self) -> Result<()> {
        self.flush_full_datagrams()?;
        if !self.pending.is_empty() {
            self.socket.send(&self.pending)?;
            self.pending.clear();
        }
        Ok(())
    }
}

fn parse_udp_url(url: &str) -> Result<SocketAddr> {
    let authority = url
        .strip_prefix("udp://")
        .ok_or_else(|| anyhow!("stream address must look like udp://host:port, got {url:?}"))?;
    let authority = authority.trim_end_matches('/');
    // Both udp://239.0.0.1:5004 and the ffmpeg style udp://@239.0.0.1:5004.
    let authority = authority.replace("@", "");

    let mut addresses = authority
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve {authority:?}"))?;
    let address = addresses
        .find(|address| matches!(address.ip(), IpAddr::V4(_)))
        .or_else(|| authority.to_socket_addrs().ok()?.next())
        .ok_or_else(|| anyhow!("{authority:?} did not resolve to an address"))?;
    if address.port() == 0 {
        return Err(anyhow!("stream address {url:?} needs a port"));
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_ffmpeg_style_urls() {
        assert_eq!(
            parse_udp_url("udp://239.0.0.1:5004").unwrap(),
            SocketAddr::from(([239, 0, 0, 1], 5004))
        );
        assert_eq!(
            parse_udp_url("udp://@239.0.0.1:5004/").unwrap(),
            SocketAddr::from(([239, 0, 0, 1], 5004))
        );
    }

    #[test]
    fn rejects_other_schemes_and_missing_ports() {
        assert!(parse_udp_url("rtp://239.0.0.1:5004").is_err());
        assert!(parse_udp_url("udp://239.0.0.1").is_err());
    }
}
