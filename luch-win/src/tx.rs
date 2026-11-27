//! Sending frames as Luch datagrams (PROTOCOL.md section 4), the way luchd does: every shard of a frame at once,
//! in bursts of 16 with a short gap so a large IDR does not overflow the Wi-Fi queue.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use luch_proto::wire::{self, HEADER, SHARD};

/// Where datagrams go: a UDP socket, or a list in tests.
pub trait Datagram {
    fn send(&self, b: &[u8]) -> io::Result<usize>;
}

impl Datagram for UdpSocket {
    fn send(&self, b: &[u8]) -> io::Result<usize> {
        UdpSocket::send(self, b)
    }
}

const BURST: usize = 16;
const BURST_GAP: Duration = Duration::from_micros(250);

/// A UDP socket to the receiver from the address of the LAN interface the receiver was found on (PROTOCOL.md
/// section 2). Windows has no SO_BINDTODEVICE; binding to the interface's address picks it, so a full-tunnel VPN's
/// default route never decides where the stream goes. DSCP needs qWAVE on Windows (`win::sys::Qos`): the IP_TOS
/// option is ignored there unless a group policy allows it.
pub fn open_udp(local: Option<Ipv4Addr>, to: SocketAddrV4) -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_send_buffer_size(1 << 20)?;
    s.bind(&SocketAddrV4::new(local.unwrap_or(Ipv4Addr::UNSPECIFIED), 0).into())?;
    s.connect(&to.into())?;
    Ok(s.into())
}

pub struct Tx<D> {
    out: D,
    kind: u8,
    seq: u32,
    warned: bool,
    pub bytes: u64,
    pub frames: u64,
}

impl<D: Datagram> Tx<D> {
    pub fn new(out: D, kind: u8) -> Self {
        Tx {
            out,
            kind,
            seq: 0,
            warned: false,
            bytes: 0,
            frames: 0,
        }
    }

    pub fn send_frame(&mut self, data: &[u8], flags: u8, pts_us: u64, host: u16) {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.bytes += data.len() as u64;
        self.frames += 1;
        let mut pkt = Vec::with_capacity(HEADER + SHARD);
        for (i, (h, chunk)) in
            wire::shards(self.kind, flags, seq, pts_us as u32, host, data).enumerate()
        {
            if i > 0 && i % BURST == 0 {
                std::thread::sleep(BURST_GAP);
            }
            pkt.clear();
            pkt.extend_from_slice(&h);
            pkt.extend_from_slice(chunk);
            // A lost datagram is repaired by the receiver's keyframe request; report only the first failure.
            if let Err(e) = self.out.send(&pkt) {
                if !std::mem::replace(&mut self.warned, true) {
                    eprintln!("[luch-win] udp send (kind {}): {e}", self.kind);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luch_proto::wire::{Assembler, Header, F_IDR, KIND_VIDEO};

    #[test]
    fn a_frame_over_udp_reassembles_on_the_receiver() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let to = match rx.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        let mut tx = Tx::new(open_udp(Some(Ipv4Addr::LOCALHOST), to).unwrap(), KIND_VIDEO);
        // Bigger than one burst, so the pause between bursts is on the way too.
        let frame: Vec<u8> = (0..SHARD * 20 + 7).map(|i| (i * 7) as u8).collect();
        tx.send_frame(&frame, F_IDR, 99, 42);
        let mut a = Assembler::new();
        let mut buf = [0u8; 2000];
        let got = loop {
            let n = rx.recv(&mut buf).unwrap();
            let (h, data) = Header::parse(&buf[..n]).unwrap();
            if let Some(f) = a.push(&h, data).frame {
                break f;
            }
        };
        assert_eq!((got.seq, got.pts_us, got.idr, got.host), (0, 99, true, 42));
        assert_eq!(got.data, frame);
        assert_eq!((tx.frames, tx.bytes), (1, frame.len() as u64));
    }

    struct Failing(std::cell::Cell<usize>);
    impl Datagram for Failing {
        fn send(&self, _: &[u8]) -> io::Result<usize> {
            self.0.set(self.0.get() + 1);
            Err(io::Error::other("no route"))
        }
    }

    #[test]
    fn a_failing_socket_does_not_stop_the_frame_and_sequence_moves_on() {
        let mut tx = Tx::new(Failing(Default::default()), KIND_VIDEO);
        tx.send_frame(&[0; SHARD * 3], 0, 0, 0);
        tx.send_frame(&[0; 10], 0, 0, 0);
        assert_eq!(tx.out.0.get(), 4, "every shard was tried");
        assert_eq!(tx.seq, 2);
        assert!(tx.warned);
    }
}
