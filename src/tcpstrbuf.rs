use crate::mio::net::TcpStream;
use crate::MioSource;
use std::io::{Error, ErrorKind, Read, Result, Write};

/// Type to aid with managing a [`mio::net::TcpStream`] along with
/// [`MioPoll`]
///
/// First create the stream and add it to [`MioPoll`], then pass the
/// resulting [`MioSource`] to [`TcpStreamBuf::init`], which allows
/// this struct to manage the buffering.
///
/// By default TCP_NODELAY is set to `false` on the stream.  However
/// if you're writing large chunks of data, it is recommended to
/// enable TCP_NODELAY by calling [`TcpStreamBuf::set_nodelay`] with
/// `true`.  This disables the Nagle algorithm, which means that the
/// last packet of a large write will arrive sooner at the
/// destination.  However if you are writing very small amounts, and
/// would benefit from the Nagle algorithm batching up data despite
/// the extra round-trip, then it's fine to leave it as `false`.
/// Since on Windows the TCP_NODELAY flag can only be changed once the
/// stream is writable, it is set on the first flush after each new
/// stream is installed.
///
/// [`MioPoll`]: struct.MioPoll.html
/// [`MioSource`]: struct.MioSource.html
/// [`TcpStreamBuf::init`]: struct.TcpStreamBuf.html#method.init
/// [`TcpStreamBuf::set_nodelay`]: struct.TcpStreamBuf.html#method.set_nodelay
/// [`mio::net::TcpStream`]: ../mio/net/struct.TcpStream.html
#[derive(Default)]
pub struct TcpStreamBuf {
    /// Output buffer.  Append data here, and then call
    /// [`TcpStreamBuf::flush`] when ready to send.  If the stream is
    /// receiving backpressure from the remote end then you'll see
    /// data here building up.
    ///
    /// [`TcpStreamBuf::flush`]: struct.TcpStreamBuf.html#method.flush
    pub out: Vec<u8>,

    /// Output EOF flag.  When this is set to `true` and the `out`
    /// buffer fully empties in a [`TcpStreamBuf::flush`] call, the
    /// outgoing half of the stream will be shut down, which signals
    /// end-of-file.  If any data is added to `out` after this point
    /// it will give an error from `flush`.
    ///
    /// [`TcpStreamBuf::flush`]: struct.TcpStreamBuf.html#method.flush
    pub out_eof: bool,

    /// Input buffer.  To receive data, read data from offset `rd` up
    /// to offset `wr`, updating `rd` offset as you go.  Call
    /// [`TcpStreamBuf::read`] to pull more data into the buffer,
    /// which will update both `rd` and `wr` offsets (dropping data
    /// before `rd`).  To apply backpressure to the remote end, use
    /// [`stakker::idle!`] for the `read()` call.
    ///
    /// [`TcpStreamBuf::read`]: struct.TcpStreamBuf.html#method.read
    /// [`stakker::idle!`]: ../stakker/macro.idle.html
    pub inp: Vec<u8>,

    /// Offset for reading in input buffer
    pub rd: usize,

    /// Offset for writing in input buffer
    pub wr: usize,

    // TCP_NODELAY flag
    nodelay: bool,

    // Pending set_nodelay()
    pending_set_nodelay: bool,

    // Set when EOF has been sent
    sent_out_eof: bool,

    // Active TCP connection, or None
    stream: Option<MioSource<TcpStream>>,
}

impl TcpStreamBuf {
    /// Create a new empty TcpStreamBuf, without any stream currently
    /// associated
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            out_eof: false,
            inp: Vec::new(),
            rd: 0,
            wr: 0,
            nodelay: false,
            pending_set_nodelay: false,
            sent_out_eof: false,
            stream: None,
        }
    }

    /// After adding a stream to the MioPoll instance with
    /// [`MioPoll::add`], store the [`MioSource`] here to handle the
    /// buffering.  `TcpStreamBuf` takes care of deregistering the
    /// stream on drop.  The caller should probably call
    /// [`TcpStreamBuf::flush`] and [`TcpStreamBuf::read`] soon after
    /// this call.
    ///
    /// [`MioPoll::add`]: struct.MioPoll.html#method.add
    /// [`MioSource`]: struct.MioSource.html
    /// [`TcpStreamBuf::flush`]: struct.TcpStreamBuf.html#method.flush
    /// [`TcpStreamBuf::read`]: struct.TcpStreamBuf.html#method.read
    pub fn init(&mut self, stream: MioSource<TcpStream>) {
        self.stream = Some(stream);
        self.sent_out_eof = false;
        self.pending_set_nodelay = true;
    }

    /// Discard the current stream if there is one, deregistering it
    /// from the [`MioPoll`] instance.
    ///
    /// [`MioPoll`]: struct.MioPoll.html
    pub fn deinit(&mut self) {
        self.stream = None;
    }

    /// Change the TCP_NODELAY setting for the stream.  Passing `true`
    /// disables the Nagle algorithm.  The change will be applied on
    /// the next flush.
    pub fn set_nodelay(&mut self, nodelay: bool) {
        if self.nodelay != nodelay {
            self.nodelay = nodelay;
            self.pending_set_nodelay = true;
        }
    }

    /// Flush as much data as possible out to the stream
    pub fn flush(&mut self) -> Result<()> {
        if let Some(ref mut stream) = self.stream {
            if self.pending_set_nodelay {
                self.pending_set_nodelay = false;
                loop {
                    match stream.set_nodelay(self.nodelay) {
                        Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                        Ok(()) => break,
                    }
                }
            }

            while !self.out.is_empty() {
                match stream.write(&self.out[..]) {
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                    Err(e) => return Err(e),
                    Ok(0) => break, // Shouldn't happen, but deal with it
                    Ok(len) => {
                        self.out.drain(..len);
                        continue;
                    }
                };
            }
            loop {
                match stream.flush() {
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => (),
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                    Err(e) => return Err(e),
                    Ok(_) => break,
                };
            }
            if self.out_eof && !self.sent_out_eof && self.out.is_empty() {
                loop {
                    match stream.shutdown(std::net::Shutdown::Write) {
                        Err(ref e) if e.kind() == ErrorKind::Interrupted => (),
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                        Err(e) => return Err(e),
                        Ok(_) => break,
                    };
                }
                self.sent_out_eof = true;
            }
        }
        Ok(())
    }

    /// Read more data and append it to the data currently in the
    /// `inp` buffer.  This is non-blocking.  Bytes before the `rd`
    /// offset might be dropped from the buffer, and `rd` might be
    /// moved.  No more than `max` bytes are read, which allows
    /// regulating the data input rate if that is required.  If you
    /// need to apply backpressure when under load, call this method
    /// from a [`stakker::idle!`] handler.  This must be called
    /// repeatedly until it returns `ReadStatus::WouldBlock` in order
    /// to get another read notification from `mio`.
    ///
    /// [`stakker::idle!`]: ../stakker/macro.idle.html
    pub fn read(&mut self, max: usize) -> ReadStatus {
        if self.rd != 0 {
            self.inp.copy_within(self.rd..self.wr, 0);
            self.wr -= self.rd;
            self.rd = 0;
        }

        if let Some(ref mut stream) = self.stream {
            // Extend buffer if required
            let end = self.wr + max;
            if self.inp.len() < end {
                self.inp.reserve(end - self.inp.len());
                self.inp.resize(self.inp.capacity(), 0);
            }

            loop {
                match stream.read(&mut self.inp[self.wr..]) {
                    Ok(0) => {
                        return ReadStatus::EndOfStream;
                    }
                    Ok(len) => {
                        self.wr += len;
                        return ReadStatus::NewData;
                    }
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                        return ReadStatus::WouldBlock;
                    }
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => {
                        continue;
                    }
                    Err(e) => {
                        return ReadStatus::Error(e);
                    }
                }
            }
        }
        ReadStatus::WouldBlock
    }
}

/// Result of a [`TcpStreamBuf::read`] operation
///
/// [`TcpStreamBuf::read`]: struct.TcpStreamBuf.html#method.read
pub enum ReadStatus {
    /// New data has been read
    NewData,
    /// No data is available at this moment
    WouldBlock,
    /// End of stream was reported
    EndOfStream,
    /// I/O error
    Error(Error),
}
