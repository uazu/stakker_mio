use crate::mio::net::UdpSocket;
use crate::MioSource;
use std::collections::VecDeque;
use std::io::{ErrorKind, Result};

/// Type to aid with managing a connected [`mio::net::UdpSocket`]
/// along with [`MioPoll`]
///
/// First create the UDP socket and call `connect()` on it, add it to
/// [`MioPoll`], then pass the resulting [`MioSource`] to
/// [`UdpQueue::init`], which allows this struct to manage the
/// queueing.  Your ready handler should call [`UdpQueue::flush`] when
/// the socket is WRITABLE, and [`UdpQueue::read`] repeatedly until no
/// more packets are available when the socket is READABLE.  For
/// example:
///
/// ```no_run
/// use stakker_mio::{MioPoll, Ready, UdpQueue, mio::Interest, mio::net::UdpSocket};
/// use std::net::{IpAddr, SocketAddr};
///# use stakker::{fwd_to, fail, CX};
///# struct MyActor { queue: UdpQueue } impl MyActor {
///
/// fn setup_queue(cx: CX![], miopoll: MioPoll, local_ip: IpAddr, remote_addr: SocketAddr)
///     -> std::io::Result<UdpQueue>
/// {
///     // Port 0 means bind to any free emphemeral port
///     let sock = UdpSocket::bind(SocketAddr::new(local_ip, 0))?;
///     sock.connect(remote_addr)?;
///     let sock = miopoll.add(sock, Interest::READABLE | Interest::WRITABLE, 10,
///                            fwd_to!([cx], udp_ready() as (Ready)))?;
///     let mut queue = UdpQueue::new();
///     queue.init(sock);
///     Ok(queue)
/// }
///
/// fn udp_ready(&mut self, cx: CX![], ready: Ready) {
///     if ready.is_readable() {
///         let mut tmp = [0; 4096];
///         loop {
///             match self.queue.read(&mut tmp) {
///                 Err(e) => return fail!(cx, "UDP recv error: {}", e),
///                 Ok(None) => break,
///                 Ok(Some(slice)) => {
///                     // ... process packet in `slice` ...
///                 }
///             }
///         }
///     }
///
///     if ready.is_writable() {
///         if let Err(e) = self.queue.flush() {
///             fail!(cx, "UDP send error: {}", e);
///         }
///     }
/// }
///# }
///```
///
/// Since the socket is assumed to be connected, no peer addresses are
/// required when sending, and no peer addresses are returned on
/// receiving.
///
/// [`MioPoll`]: struct.MioPoll.html
/// [`MioSource`]: struct.MioSource.html
/// [`UdpQueue::flush`]: struct.UdpQueue.html#method.flush
/// [`UdpQueue::init`]: struct.UdpQueue.html#method.init
/// [`UdpQueue::read`]: struct.UdpQueue.html#method.read
/// [`mio::net::UdpSocket`]: ../mio/net/struct.UdpSocket.html
#[derive(Default)]
pub struct UdpQueue {
    /// Output queue.  To send, append packets here using
    /// `out.push_back()` or the [`UdpQueue::push`] call and then call
    /// [`UdpQueue::flush`].  If the OS buffer is full, then you'll
    /// see packets building up here.  However unlike TCP, UDP has no
    /// mechanism for backpressure, so any buildup should clear
    /// quickly as the OS moves the packets out of its own buffers and
    /// indicates WRITABLE again.
    ///
    /// [`UdpQueue::flush`]: struct.UdpQueue.html#method.flush
    /// [`UdpQueue::push`]: struct.UdpQueue.html#method.push
    pub out: VecDeque<Vec<u8>>,

    // Active UDP socket, or None
    socket: Option<MioSource<UdpSocket>>,
}

impl UdpQueue {
    /// Create a new empty [`UdpQueue`], without any UDP socket
    /// currently associated
    ///
    /// [`UdpQueue`]: struct.UdpQueue.html
    pub fn new() -> Self {
        Self {
            out: VecDeque::new(),
            socket: None,
        }
    }

    /// After adding a socket to the MioPoll instance with
    /// [`MioPoll::add`], store the [`MioSource`] here to handle the
    /// buffering.  [`UdpQueue`] takes care of deregistering the
    /// stream on drop.  The caller should probably call
    /// [`UdpQueue::flush`] and [`UdpQueue::read`] soon after this
    /// call.
    ///
    /// [`MioPoll::add`]: struct.MioPoll.html#method.add
    /// [`MioSource`]: struct.MioSource.html
    /// [`UdpQueue::flush`]: struct.UdpQueue.html#method.flush
    /// [`UdpQueue::read`]: struct.UdpQueue.html#method.read
    /// [`UdpQueue`]: struct.UdpQueue.html
    pub fn init(&mut self, socket: MioSource<UdpSocket>) {
        self.socket = Some(socket);
    }

    /// Discard the current socket if there is one, deregistering it
    /// from the [`MioPoll`] instance.
    ///
    /// [`MioPoll`]: struct.MioPoll.html
    pub fn deinit(&mut self) {
        self.socket = None;
    }

    /// Add a packet to the queue.  You must call [`UdpQueue::flush`]
    /// soon after.
    ///
    /// [`UdpQueue::flush`]: struct.UdpQueue.html#method.flush
    pub fn push(&mut self, packet: Vec<u8>) {
        self.out.push_back(packet);
    }

    /// Flush as many packets out as possible
    pub fn flush(&mut self) -> Result<()> {
        if let Some(ref mut socket) = self.socket {
            while let Some(pk) = self.out.pop_front() {
                loop {
                    return match socket.send(&pk) {
                        Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => Ok(()),
                        Err(e) => Err(e),
                        Ok(len) if len != pk.len() => {
                            // Not exactly the correct error, but has similar meaning
                            Err(ErrorKind::WriteZero.into())
                        }
                        Ok(_) => break,
                    };
                }
            }
        }
        Ok(())
    }

    /// Read in a packet, if available.  This call is non-blocking.
    /// `tmp` is a scratch buffer used for fetching packets, which
    /// should be larger than the largest packet expected.  If a
    /// packet is too large, it will be truncated.  The largest
    /// possible UDP packet is just under 64KiB.  `tmp` may be a
    /// reference to an array on the stack, or you might wish to keep
    /// a temporary buffer permanently allocated somewhere if you need
    /// it to be large and avoid the cost of zeroing a buffer
    /// regularly.
    ///
    /// On reading a packet, `Ok(Some(slice))` is returned, where
    /// `slice` is a reference into `tmp`.  If there is no packet
    /// available, `Ok(None)` is returned.  This method must be called
    /// repeatedly until it returns `Ok(None)`.  Only then will `mio`
    /// be primed to send a new READABLE ready-notification.
    ///
    /// Packets are intentionally fetched one at a time, which allows
    /// regulating the input rate if that is required, perhaps in
    /// combination with [`stakker::idle!`].  Since there is no
    /// backpressure in UDP, if you allow too much data to build up in
    /// the OS queue then the OS may drop packets.  However dropping
    /// them in the OS queue is more efficient than loading them into
    /// memory and having to drop them there.
    ///
    /// [`stakker::idle!`]: ../stakker/macro.idle.html
    pub fn read<'a>(&mut self, tmp: &'a mut [u8]) -> Result<Option<&'a [u8]>> {
        if let Some(ref mut socket) = self.socket {
            loop {
                return match socket.recv(tmp) {
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
                    Err(ref e)
                        if e.kind() == ErrorKind::Other && e.raw_os_error() == Some(10040) =>
                    {
                        // On Windows this means that the read
                        // succeeded but the packet was truncated
                        Ok(Some(tmp))
                    }
                    Err(e) => Err(e),
                    Ok(len) => Ok(Some(&tmp[..len])),
                };
            }
        }
        Ok(None)
    }

    /// Read in a packet, if available.  Works as for
    /// [`UdpQueue::read`], except that the data is copied into a new
    /// `Vec`.
    ///
    /// [`UdpQueue::read`]: struct.UdpQueue.html#method.read
    pub fn read_to_vec(&mut self, tmp: &mut [u8]) -> Result<Option<Vec<u8>>> {
        match self.read(tmp) {
            Ok(Some(slice)) => Ok(Some(slice.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
