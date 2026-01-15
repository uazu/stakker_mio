use crate::{mio::net::UdpSocket, mio::Interest, MioPoll, Ready, UdpServerQueue};
use stakker::{actor, call, fail, fwd_to, ret_shutdown, stop, Actor, StopCause, CX};
use std::net::Ipv4Addr;
use std::net::{SocketAddr, SocketAddrV4};

/// Test opening two UDP sockets and passing data, but without
/// connecting them.  So both sending and receiving passes the
/// address.
pub fn test_udpserver() {
    let mut stakker = super::init();
    let s = &mut stakker;

    let send_sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let recv_sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let send_sock_addr = send_sock.local_addr().unwrap();
    let recv_sock_addr = recv_sock.local_addr().unwrap();

    let send = actor!(s, Sender::init(send_sock), ret_shutdown!(s));
    let _recv = actor!(
        s,
        Receiver::init(recv_sock, send.clone(), send_sock_addr, recv_sock_addr),
        ret_shutdown!(s)
    );

    super::run(s).expect("I/O failure");

    let reason = s.shutdown_reason().unwrap();
    assert!(
        matches!(reason, StopCause::Stopped),
        "Actor failed: {}",
        reason
    );
}

/// Actor which sends test data
struct Sender {
    queue: UdpServerQueue,
}

impl Sender {
    fn init(cx: CX![], socket: UdpSocket) -> Option<Self> {
        let mut this = Sender {
            queue: UdpServerQueue::new(),
        };
        if let Err(e) = this.init_aux(cx, socket) {
            fail!(cx, "Failed to create listening socket: {}", e);
            None
        } else {
            Some(this)
        }
    }

    fn init_aux(&mut self, cx: CX![], socket: UdpSocket) -> std::io::Result<()> {
        let miopoll = cx.anymap_get::<MioPoll>();
        let source = miopoll.add(
            socket,
            Interest::WRITABLE,
            10,
            fwd_to!([cx], ready() as (Ready)),
        )?;
        self.queue.init(source);
        Ok(())
    }

    fn ready(&mut self, cx: CX![], _: Ready) {
        self.flush(cx);
    }

    fn flush(&mut self, cx: CX![]) {
        if let Err(e) = self.queue.flush() {
            fail!(cx, "Failed to flush queue: {}", e);
        }
    }

    fn send_data(&mut self, cx: CX![], addr: SocketAddr, data: Vec<u8>) {
        println!("Sending {} bytes", data.len());
        self.queue.push(addr, data);
        self.flush(cx);
    }
}

// These sizes must not be oversized for the receive buffer because
// behaviour on Windows and UNIX vary in that case
const TEST_SIZES: [usize; 7] = [999, 100, 1023, 410, 600, 200, 751];

struct Receiver {
    queue: UdpServerQueue,
    next_index: usize,
    expecting: Option<Vec<u8>>,
    sender: Actor<Sender>,
    send_addr: SocketAddr,
    recv_addr: SocketAddr,
}

impl Receiver {
    fn init(
        cx: CX![],
        socket: UdpSocket,
        sender: Actor<Sender>,
        send_addr: SocketAddr,
        recv_addr: SocketAddr,
    ) -> Option<Self> {
        let mut this = Self {
            queue: UdpServerQueue::new(),
            next_index: 0,
            expecting: None,
            sender,
            send_addr,
            recv_addr,
        };
        if let Err(e) = this.init_aux(cx, socket) {
            fail!(cx, "Failed to create listening socket: {}", e);
            None
        } else {
            Some(this)
        }
    }

    fn init_aux(&mut self, cx: CX![], socket: UdpSocket) -> std::io::Result<()> {
        let miopoll = cx.anymap_get::<MioPoll>();
        let source = miopoll.add(
            socket,
            Interest::READABLE,
            10,
            fwd_to!([cx], ready() as (Ready)),
        )?;
        self.queue.init(source);

        call!([cx], next_packet());

        Ok(())
    }

    fn ready(&mut self, cx: CX![], _: Ready) {
        if let Err(e) = self.ready_aux(cx) {
            fail!(cx, "Read packet failed: {}", e);
        }
    }

    fn ready_aux(&mut self, cx: CX![]) -> std::io::Result<()> {
        let mut buf = [0; 1024];
        while let Some((addr, slice)) = self.queue.read(&mut buf)? {
            println!("Received {} bytes from {}", slice.len(), addr);
            if addr != self.send_addr {
                continue;
            }
            if let Some(exp) = self.expecting.take() {
                assert_ne!(slice.len(), 1024, "Not expecting data to be truncated");
                assert_eq!(exp, slice, "Received data doesn't match expected");
                self.next_packet(cx);
            } else {
                panic!("Received a packet, but no data expected: {:?}", slice);
            }
        }
        Ok(())
    }

    fn next_packet(&mut self, cx: CX![]) {
        if self.next_index >= TEST_SIZES.len() {
            println!("Stopping");
            stop!(cx);
        } else {
            let data = super::testdata(TEST_SIZES[self.next_index], self.next_index as u32);
            self.next_index += 1;
            call!([self.sender], send_data(self.recv_addr, data.clone()));
            self.expecting = Some(data);
        }
    }
}
