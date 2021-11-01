use crate::mio::net::UdpSocket;
use crate::MioSource;
use std::collections::VecDeque;
use std::io::{ErrorKind, Result};
use std::net::SocketAddr;

/// Type to aid with managing a server
/// [`mio::net::UdpSocket`] along with [`MioPoll`]
///
/// Unlike [`UdpQueue`], this is for sockets that are not connected,
/// so it accepts UDP packets from any address/port, and may send UDP
/// packets to any address/port.
///
/// First create the UDP socket, add it to [`MioPoll`], then pass the
/// resulting [`MioSource`] to [`UdpServerQueue::init`], which allows this
/// struct to manage the queueing.  Your ready handler should call
/// [`UdpServerQueue::flush`] when the socket is WRITABLE, and
/// [`UdpServerQueue::read`] repeatedly until no more packets are available
/// when the socket is READABLE.
///
/// ```no_run
/// use stakker_mio::{MioPoll, Ready, UdpServerQueue, mio::Interest, mio::net::UdpSocket};
/// use std::net::{IpAddr, SocketAddr};
///# use stakker::{fwd_to, fail, CX};
///# struct MyActor { queue: UdpServerQueue } impl MyActor {
///
/// fn setup_queue(cx: CX![], miopoll: MioPoll, local_addr: SocketAddr)
///     -> std::io::Result<UdpServerQueue>
/// {
///     let sock = UdpSocket::bind(local_addr)?;
///     let sock = miopoll.add(sock, Interest::READABLE | Interest::WRITABLE, 10,
///                            fwd_to!([cx], udp_ready() as (Ready)))?;
///     let mut queue = UdpServerQueue::new();
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
///                 Ok(Some((addr, slice))) => {
///                     // ... process packet in `slice` from source `addr` ...
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
/// [`MioPoll`]: struct.MioPoll.html
/// [`MioSource`]: struct.MioSource.html
/// [`UdpQueue`]: struct.UdpQueue.html
/// [`UdpServerQueue::flush`]: struct.UdpServerQueue.html#method.flush
/// [`UdpServerQueue::init`]: struct.UdpServerQueue.html#method.init
/// [`UdpServerQueue::read`]: struct.UdpServerQueue.html#method.read
/// [`mio::net::UdpSocket`]: ../mio/net/struct.UdpSocket.html
#[derive(Default)]
pub struct UdpServerQueue {
    /// Output queue.  To send, append packets here using
    /// `out.push_back()` or the [`UdpServerQueue::push`] call and then call
    /// [`UdpServerQueue::flush`].  If the OS buffer is full, then you'll
    /// see packets building up here.  However unlike TCP, UDP has no
    /// mechanism for backpressure, so any buildup should clear
    /// quickly as the OS moves the packets out of its own buffers and
    /// indicates WRITABLE again.
    ///
    /// [`UdpServerQueue::flush`]: struct.UdpServerQueue.html#method.flush
    /// [`UdpServerQueue::push`]: struct.UdpServerQueue.html#method.push
    pub out: VecDeque<(SocketAddr, Vec<u8>)>,

    // Active UDP socket, or None
    socket: Option<MioSource<UdpSocket>>,
}

impl UdpServerQueue {
    /// Create a new empty [`UdpServerQueue`], without any UDP socket
    /// currently associated
    ///
    /// [`UdpServerQueue`]: struct.UdpServerQueue.html
    pub fn new() -> Self {
        Self {
            out: VecDeque::new(),
            socket: None,
        }
    }

    /// After adding a socket to the MioPoll instance with
    /// [`MioPoll::add`], store the [`MioSource`] here to handle the
    /// buffering.  [`UdpServerQueue`] takes care of deregistering the
    /// stream on drop.  The caller should probably call
    /// [`UdpServerQueue::flush`] and [`UdpServerQueue::read`] soon
    /// after this call.
    ///
    /// [`MioPoll::add`]: struct.MioPoll.html#method.add
    /// [`MioSource`]: struct.MioSource.html
    /// [`UdpServerQueue::flush`]: struct.UdpServerQueue.html#method.flush
    /// [`UdpServerQueue::read`]: struct.UdpServerQueue.html#method.read
    /// [`UdpServerQueue`]: struct.UdpServerQueue.html
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

    /// Add a packet to the queue to send to the given address.  You
    /// must call [`UdpServerQueue::flush`] soon after.
    ///
    /// [`UdpServerQueue::flush`]: struct.UdpServerQueue.html#method.flush
    pub fn push(&mut self, addr: SocketAddr, packet: Vec<u8>) {
        self.out.push_back((addr, packet));
    }

    /// Flush as many packets out as possible
    pub fn flush(&mut self) -> Result<()> {
        if let Some(ref mut socket) = self.socket {
            while let Some((addr, pk)) = self.out.pop_front() {
                loop {
                    return match socket.send_to(&pk, addr) {
                        Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => Ok(()),
                        Err(e) => Err(e),
                        Ok(len) if len != pk.len() => {
                            // Not exactly the correct error, but has similar meaning
                            Err(ErrorKind::WriteZero.into())
                        }
                        Ok(_) => Ok(()),
                    };
                }
            }
        }
        Ok(())
    }

    /// Read in a packet, if available.  This call is non-blocking.
    /// `tmp` is a scratch buffer used for fetching packets, which
    /// should be larger than the largest packet expected.  If a
    /// packet is too large, it will either be truncated (UNIX) or
    /// discarded (Windows).  The largest possible UDP packet is just
    /// under 64KiB.  `tmp` may be a reference to an array on the
    /// stack, or you might wish to keep a temporary buffer
    /// permanently allocated somewhere if you need it to be large and
    /// avoid the cost of zeroing a buffer regularly.
    ///
    /// On reading a packet, `Ok(Some((addr, slice)))` is returned,
    /// where `slice` is a reference into `tmp`, and `addr` is the
    /// source `SocketAddr`.  If there is no packet available,
    /// `Ok(None)` is returned.  This method must be called repeatedly
    /// until it returns `Ok(None)`.  Only then will `mio` be primed
    /// to send a new READABLE ready-notification.
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
    pub fn read<'a>(&mut self, tmp: &'a mut [u8]) -> Result<Option<(SocketAddr, &'a [u8])>> {
        if let Some(ref mut socket) = self.socket {
            loop {
                return match socket.recv_from(tmp) {
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
                    Err(ref e)
                        if e.kind() == ErrorKind::Other && e.raw_os_error() == Some(10040) =>
                    {
                        // On Windows this means that the read
                        // succeeded but the packet was truncated.
                        // However we can't return anything because we
                        // don't have the source address.  So the only
                        // option is to skip it.
                        continue;
                    }
                    Err(e) => Err(e),
                    Ok((len, addr)) => Ok(Some((addr, &tmp[..len]))),
                };
            }
        }
        Ok(None)
    }

    /// Read in a packet, if available.  Works as for
    /// [`UdpServerQueue::read`], except that the data is copied into
    /// a new `Vec`.
    ///
    /// [`UdpServerQueue::read`]: struct.UdpServerQueue.html#method.read
    pub fn read_to_vec(&mut self, tmp: &mut [u8]) -> Result<Option<(SocketAddr, Vec<u8>)>> {
        match self.read(tmp) {
            Ok(Some((addr, slice))) => Ok(Some((addr, slice.to_vec()))),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
