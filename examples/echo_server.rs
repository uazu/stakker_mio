//! This demonstrates a TCP echo server.  It listens for connections
//! on port 7777.  Each connection is handled by a separate actor.
//! This actor just sends back whatever data it receives over TCP.
//!
//! However to make things a little more interesting, some additional
//! processing is performed:
//!
//! - All echos are delayed by one second.
//!
//! - If the character '!' is passed, then the actor terminates the
//!   TCP connection and shuts down.  All other TCP connections
//!   continue as normal though.
//!
//! - If the character '%' is passed, this causes the actor to fail,
//!   passing an `AbortError` back to the listener.  The listener
//!   detects this particular kind of failure, and shuts down the
//!   whole server.
//!
//! - The server will shut down if there is no incoming connection for
//!   60 seconds
//!
//! Start the example, and then connect using `telnet 127.0.0.1 7777`.
//! Many `telnet` sessions can be handled at the same time.

use stakker::{
    actor, actor_in_slab, after, fail, fwd_to, ret_shutdown, ret_some_to, stop, timer_max,
    ActorOwnSlab, MaxTimerKey, Stakker, StopCause, CX,
};
use stakker_mio::mio::net::{TcpListener, TcpStream};
use stakker_mio::mio::{Events, Interest, Poll};
use stakker_mio::{MioPoll, MioSource, ReadStatus, Ready, TcpStreamBuf};

use std::error::Error;
use std::fmt;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

const PORT: u16 = 7777;

// Here fatal top-level MIO failures are returned from main.  All
// other I/O failures are handled as actor failure.
fn main() -> Result<(), Box<dyn Error>> {
    let mut stakker = Stakker::new(Instant::now());
    let s = &mut stakker;
    let miopoll = MioPoll::new(s, Poll::new()?, Events::with_capacity(1024), 10)?;

    let _listener = actor!(s, Listener::init(), ret_shutdown!(s));

    // Don't need `idle!` handling, but run a full event loop in case
    // someone wants to copy this
    let mut idle_pending = s.run(Instant::now(), false);
    let mut io_pending = false;
    let mut activity;
    const MAX_WAIT: Duration = Duration::from_secs(60);
    while s.not_shutdown() {
        let maxdur = s.next_wait_max(Instant::now(), MAX_WAIT, idle_pending || io_pending);
        (activity, io_pending) = miopoll.poll(maxdur)?;
        idle_pending = s.run(Instant::now(), !activity);
    }

    println!("Shutdown: {}", s.shutdown_reason().unwrap());
    Ok(())
}

/// Listens for incoming TCP connections
struct Listener {
    children: ActorOwnSlab<Echoer>,
    listener: MioSource<TcpListener>,
    inactivity: MaxTimerKey,
}

impl Listener {
    fn init(cx: CX![]) -> Option<Self> {
        match Self::setup(cx) {
            Err(e) => {
                fail!(cx, "Listening socket setup failed on port {}: {}", PORT, e);
                None
            }
            Ok(this) => Some(this),
        }
    }

    fn setup(cx: CX![]) -> std::io::Result<Self> {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, PORT);
        let listen = TcpListener::bind(SocketAddr::V4(addr))?;
        let miopoll = cx.anymap_get::<MioPoll>();
        let listener = miopoll.add(
            listen,
            Interest::READABLE,
            10,
            fwd_to!([cx], connect() as (Ready)),
        )?;
        println!("Listening on port 7777 for incoming telnet connections ...");

        let mut this = Self {
            listener,
            children: ActorOwnSlab::new(),
            inactivity: MaxTimerKey::default(),
        };
        this.activity(cx);

        Ok(this)
    }

    // Register activity, pushing back the inactivity timer
    fn activity(&mut self, cx: CX![]) {
        timer_max!(
            &mut self.inactivity,
            cx.now() + Duration::from_secs(60),
            [cx],
            |_this, cx| {
                fail!(cx, "Timed out waiting for connection");
            }
        );
    }

    fn connect(&mut self, cx: CX![], _: Ready) {
        loop {
            match self.listener.accept() {
                Ok((stream, addr)) => {
                    println!("New connection from {}", addr);
                    actor_in_slab!(
                        self.children,
                        cx,
                        Echoer::init(stream),
                        ret_some_to!([cx], |_this, cx, cause: StopCause| {
                            // Mostly just report child failure, but watch out for
                            // AbortError to terminate this actor, which in turn shuts
                            // down the whole process
                            println!("Child actor terminated: {}", cause);

                            if let StopCause::Failed(e) = cause {
                                if e.downcast::<AbortError>().is_ok() {
                                    fail!(cx, "Aborted");
                                }
                            }
                        })
                    );
                    self.activity(cx);
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    fail!(cx, "TCP listen socket failure on accept: {}", e);
                    return;
                }
            }
        }
    }
}

/// Echoes received data back to sender, with a delay
struct Echoer {
    tcp: TcpStreamBuf,
}

impl Echoer {
    fn init(cx: CX![], stream: TcpStream) -> Option<Self> {
        match Self::setup(cx, stream) {
            Err(e) => {
                fail!(cx, "Failed to set up a new TCP stream: {}", e);
                None
            }
            Ok(this) => Some(this),
        }
    }

    fn setup(cx: CX![], stream: TcpStream) -> std::io::Result<Self> {
        let miopoll = cx.anymap_get::<MioPoll>();
        let source = miopoll.add(
            stream,
            Interest::READABLE | Interest::WRITABLE,
            10,
            fwd_to!([cx], ready() as (Ready)),
        )?;

        let mut tcp = TcpStreamBuf::new();
        tcp.init(source);

        Ok(Self { tcp })
    }

    fn ready(&mut self, cx: CX![], ready: Ready) {
        if ready.is_readable() {
            loop {
                match self.tcp.read(8192) {
                    ReadStatus::NewData => {
                        let data = self.tcp.inp[self.tcp.rd..self.tcp.wr].to_vec();
                        self.tcp.rd = self.tcp.wr;
                        self.check_special_chars(cx, &data);
                        after!(Duration::from_secs(1), [cx], send_data(data));
                        continue;
                    }
                    ReadStatus::WouldBlock => (),
                    ReadStatus::EndOfStream => {
                        after!(Duration::from_secs(1), [cx], send_eof());
                    }
                    ReadStatus::Error(e) => {
                        fail!(cx, "Read failure on TCP stream: {}", e);
                    }
                }
                break;
            }
        }

        if ready.is_writable() {
            self.flush(cx);
        }
    }

    fn send_data(&mut self, cx: CX![], data: Vec<u8>) {
        self.tcp.out.extend_from_slice(&data);
        self.flush(cx);
    }

    fn send_eof(&mut self, cx: CX![]) {
        self.tcp.out_eof = true;
        self.flush(cx);
    }

    fn flush(&mut self, cx: CX![]) {
        if let Err(e) = self.tcp.flush() {
            fail!(cx, "Write failure on TCP stream: {}", e);
        }
        if self.tcp.out_eof && self.tcp.out.is_empty() {
            stop!(cx); // Stop actor when output is complete
        }
    }

    fn check_special_chars(&mut self, cx: CX![], data: &[u8]) {
        if data.contains(&b'!') {
            self.tcp.out_eof = true;
            self.flush(cx);
        }
        if data.contains(&b'%') {
            fail!(cx, AbortError);
        }
    }
}

#[derive(Debug)]
struct AbortError;
impl Error for AbortError {}
impl fmt::Display for AbortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AbortError")
    }
}
