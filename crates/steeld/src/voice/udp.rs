//! UDP side of voice: IP discovery and the media relay.
//!
//! Discovery: the client sends a 74-byte probe carrying its SSRC. The server replies with its
//! view of the client's public address and port, and records that address for the SSRC.
//! Media: each datagram from a registered address is forwarded unchanged to the other participants
//! of the same call. The server never decrypts it.

use std::{net::SocketAddr, sync::Arc};

use tokio::net::UdpSocket;

use super::Registry;

const DISCOVERY_LEN: usize = 74;
/// Keepalive from the client: a fixed 8-byte prefix, then a sequence number.
const KEEPALIVE_PREFIX: [u8; 2] = [0x13, 0x37];

pub async fn serve(socket: UdpSocket, registry: Arc<Registry>) {
    let mut buf = vec![0u8; 2048];
    loop {
        let Ok((n, src)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        let packet = &buf[..n];

        if n == DISCOVERY_LEN && packet[0..2] == [0, 1] {
            let ssrc = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
            if registry.bind_addr(ssrc, src).is_some() {
                let reply = discovery_reply(ssrc, src);
                let _ = socket.send_to(&reply, src).await;
            }
            continue;
        }
        if n == 8 && packet[0..2] == KEEPALIVE_PREFIX {
            let _ = socket.send_to(packet, src).await;
            continue;
        }
        // RTP and RTCP both need at least a fixed header.
        if n < 12 {
            continue;
        }
        if let Some(targets) = registry.route(src) {
            for target in targets {
                let _ = socket.send_to(packet, target).await;
            }
        }
    }
}

/// Build the 74-byte discovery reply: type 2, length 70, SSRC, the client's address as text
/// (null-padded to 64 bytes), and the client's port in big-endian at offset 72.
pub fn discovery_reply(ssrc: u32, client: SocketAddr) -> [u8; DISCOVERY_LEN] {
    let mut reply = [0u8; DISCOVERY_LEN];
    reply[0..2].copy_from_slice(&[0, 2]);
    reply[2..4].copy_from_slice(&70u16.to_be_bytes());
    reply[4..8].copy_from_slice(&ssrc.to_be_bytes());
    let ip = client.ip().to_string();
    let ip = ip.as_bytes();
    reply[8..8 + ip.len().min(63)].copy_from_slice(&ip[..ip.len().min(63)]);
    reply[72..74].copy_from_slice(&client.port().to_be_bytes());
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_reply_layout() {
        let client: SocketAddr = "203.0.113.7:51234".parse().unwrap();
        let reply = discovery_reply(0xdead_beef, client);
        assert_eq!(&reply[0..4], &[0, 2, 0, 70]);
        assert_eq!(&reply[4..8], &0xdead_beefu32.to_be_bytes());
        assert_eq!(&reply[8..8 + 12], b"203.0.113.7\0");
        assert_eq!(&reply[72..74], &51234u16.to_be_bytes());
    }
}
