use crate::{
    mio::net::UdpSocket,
    mio::{Events, Interest, Poll},
    MioPoll, Ready, UdpQueue,
};
use stakker::{actor, call, fail, fwd_to, ret_shutdown, stop, Actor, Stakker, StopCause, CX};
use std::net::Ipv4Addr;
use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

// TODO: Add test that shows UDP packet loss due to overflowing OS
// buffer

/// Generate random-looking test data for the given length and seed
fn testdata(len: usize, mut seed: u32) -> Vec<u8> {
    seed = (seed ^ ((len * 19) as u32)) & 0xFFFF;
    let mut out = Vec::new();
    for _ in 0..len {
        out.push(seed as u8);
        seed = ((seed + 1) * 75) % 65537 - 1;
    }
    out
}

/// Initialise stakker/miopoll system
fn init() -> Stakker {
    let mut stakker = Stakker::new(Instant::now());
    MioPoll::new(
        &mut stakker,
        Poll::new().expect("Poll::new failed"),
        Events::with_capacity(1024),
        0, // Wake priority
    )
    .expect("MioPoll::new failed");
    stakker
}

/// Run event loop.  Don't need timers or idle queue for this test.
fn run(s: &mut Stakker) {
    let miopoll = s.anymap_get::<MioPoll>();
    let now = Instant::now();
    s.run(now, false);
    while s.not_shutdown() {
        if let Err(e) = miopoll.poll(Duration::from_secs(1)) {
            panic!("MioPoll failure: {}", e);
        }
        s.run(now, false);
    }
}

/// Test opening two UDP sockets and passing data.  Also tests
/// truncation of data when a too-small receive buffer is provided.
#[test]
fn basic() {
    let mut stakker = init();
    let s = &mut stakker;

    let send_sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let recv_sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let send_sock_addr = send_sock.local_addr().unwrap();
    let recv_sock_addr = recv_sock.local_addr().unwrap();
    send_sock.connect(recv_sock_addr).unwrap();
    recv_sock.connect(send_sock_addr).unwrap();

    let send = actor!(s, Sender::init(send_sock), ret_shutdown!(s));
    let _recv = actor!(s, Receiver::init(recv_sock, send.clone()), ret_shutdown!(s));

    run(s);

    let reason = s.shutdown_reason().unwrap();
    assert!(
        matches!(reason, StopCause::Stopped),
        "Actor failed: {}",
        reason
    );
}

/// Actor which sends test data
struct Sender {
    queue: UdpQueue,
}

impl Sender {
    fn init(cx: CX![], socket: UdpSocket) -> Option<Self> {
        let mut this = Sender {
            queue: UdpQueue::new(),
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

    fn send_data(&mut self, cx: CX![], data: Vec<u8>) {
        println!("Sending {} bytes", data.len());
        self.queue.push(data);
        self.flush(cx);
    }
}

// Some of these are oversized for receive buffer, intentionally
const TEST_SIZES: [usize; 7] = [999, 100, 1234, 410, 600, 2000, 751];

struct Receiver {
    queue: UdpQueue,
    next_index: usize,
    expecting: Option<Vec<u8>>,
    sender: Actor<Sender>,
}

impl Receiver {
    fn init(cx: CX![], socket: UdpSocket, sender: Actor<Sender>) -> Option<Self> {
        let mut this = Self {
            queue: UdpQueue::new(),
            next_index: 0,
            expecting: None,
            sender,
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
        while let Some(slice) = self.queue.read(&mut buf)? {
            println!("Received {} bytes", slice.len());
            if let Some(exp) = self.expecting.take() {
                assert_eq!(
                    &exp[..exp.len().min(1024)],
                    slice,
                    "Received data doesn't match expected"
                );
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
            let data = testdata(TEST_SIZES[self.next_index], self.next_index as u32);
            self.next_index += 1;
            call!([self.sender], send_data(data.clone()));
            self.expecting = Some(data);
        }
    }
}
