use crate::mio::net::TcpStream;
use crate::MioSource;
use std::io::{Error, ErrorKind, Read, Result, Write};

/// Type to aid with managing a [`mio::net::TcpStream`] along with
/// [`MioPoll`]
///
/// First create the stream and add it to [`MioPoll`], then pass the
/// resulting [`MioSource`] to [`TcpStreamBuf::init`], which allows
/// this struct to manage the buffering.  Your ready handler should
/// call [`TcpStreamBuf::flush`] when the socket is WRITABLE, and
/// [`TcpStreamBuf::read`] when the socket is READABLE, although you
/// might want to delay some of the read calls using
/// [`stakker::idle!`] if you wish to implement backpressure.
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
/// The output buffer is a simple `Vec`, because the pattern of
/// behaviour is expected to be that it will generally be flushed out
/// in its entirety.  However the input buffer is a pre-filled `Vec`
/// with separate read and write offsets.  Data is received from the
/// TCP stream to advance the write offset.  It is expected that data
/// will be grabbed from the read offset in the buffer by the
/// application as soon as an entire line or entire record is
/// available (depending on the protocol), advancing the read offset.
/// This means it will often be necessary to leave an incomplete line
/// or record in the buffer until more data has been read from the TCP
/// stream to complete it.  So using offsets saves some copying.
///
/// [`MioPoll`]: struct.MioPoll.html
/// [`MioSource`]: struct.MioSource.html
/// [`TcpStreamBuf::flush`]: struct.TcpStreamBuf.html#method.flush
/// [`TcpStreamBuf::init`]: struct.TcpStreamBuf.html#method.init
/// [`TcpStreamBuf::read`]: struct.TcpStreamBuf.html#method.read
/// [`TcpStreamBuf::set_nodelay`]: struct.TcpStreamBuf.html#method.set_nodelay
/// [`mio::net::TcpStream`]: ../mio/net/struct.TcpStream.html
/// [`stakker::idle!`]: ../stakker/macro.idle.html
#[derive(Default)]
pub struct TcpStreamBuf {
    /// Output buffer.  Append data here, and then call
    /// [`TcpStreamBuf::flush`] when ready to send.  If the stream is
    /// receiving backpressure from the remote end then you'll see
    /// data here building up.
    ///
    /// `TcpStreamBuf` has a `Write` trait implementation which may be
    /// used to write data to this buffer.  Flushing via the `Write`
    /// trait does not flush to the TCP end-point, though.
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
    /// to offset `wr`, updating the `rd` offset as you go.  Call
    /// [`TcpStreamBuf::read`] to pull more data into the buffer,
    /// which will update `wr` offset and also possibly the `rd`
    /// offset (to drop unneeded data before `rd`).  To apply
    /// backpressure to the remote end, use [`stakker::idle!`] for the
    /// `read()` call.
    ///
    /// `TcpStreamBuf` has a `Read` trait implementation which may be
    /// used to read data from this buffer, updating the `rd` offset
    /// accordingly.
    ///
    /// [`TcpStreamBuf::read`]: struct.TcpStreamBuf.html#method.read
    /// [`stakker::idle!`]: ../stakker/macro.idle.html
    pub inp: Vec<u8>,

    /// Input EOF flag.  This is set by the [`TcpStreamBuf::read`]
    /// call when it returns `ReadStatus::EndOfStream`.  The
    /// application should process the EOF only once it has finished
    /// reading any data remaining in the `inp` buffer.
    ///
    /// [`TcpStreamBuf::read`]: struct.TcpStreamBuf.html#method.read
    pub inp_eof: bool,

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

    // Pause writes?
    pause: bool,

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
            inp_eof: false,
            rd: 0,
            wr: 0,
            nodelay: false,
            pending_set_nodelay: false,
            sent_out_eof: false,
            pause: false,
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

    /// Pause or unpause writes made by the [`TcpStreamBuf::flush`]
    /// call.  If `pause` is set to `true`, then
    /// [`TcpStreamBuf::flush`] does nothing.  Initially it is set to
    /// `false` which allows writes.  This may be used on Windows
    /// where writes will fail if attempted before the first "ready
    /// for write" indication is received from `mio`.
    ///
    /// [`TcpStreamBuf::flush`]: struct.TcpStreamBuf.html#method.flush
    pub fn pause_writes(&mut self, pause: bool) {
        self.pause = pause;
    }

    /// Flush as much data as possible out to the stream
    pub fn flush(&mut self) -> Result<()> {
        if self.pause {
            return Ok(());
        }

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

    /// Make space in the `inp` Vec to read in up to `max` bytes of
    /// data in addition to whatever is already there.  Tries to avoid
    /// unnecessary copies.
    pub fn inp_makespace(&mut self, max: usize) {
        if self.rd == self.wr {
            self.rd = 0;
            self.wr = 0;
        }

        if self.wr + max <= self.inp.len() {
            return;
        }

        if self.rd != 0 {
            self.inp.copy_within(self.rd..self.wr, 0);
            self.wr -= self.rd;
            self.rd = 0;
            if self.wr + max <= self.inp.len() {
                return;
            }
        }

        // If we make it at least `max*2` long, then that should
        // reduce the amount of copying and reallocation
        let end = (self.wr + max).max(max * 2);
        if self.inp.len() < end {
            self.inp.reserve(end - self.inp.len());
            self.inp.resize(self.inp.capacity(), 0);
        }
    }

    /// Read more data and append it to the data currently in the
    /// `inp` buffer.  This is non-blocking.  Bytes before the `rd`
    /// offset might be dropped from the buffer, and `rd` might be
    /// moved.  No more than `max` bytes are read, which allows
    /// regulating the data input rate if that is required.  If you
    /// need to apply backpressure when under load, call this method
    /// from a [`stakker::idle!`] handler.  This must be called
    /// repeatedly until it returns `ReadStatus::WouldBlock` in order
    /// to get another READABLE ready-notification from `mio`.
    ///
    /// [`stakker::idle!`]: ../stakker/macro.idle.html
    pub fn read(&mut self, max: usize) -> ReadStatus {
        // Extend buffer if required
        self.inp_makespace(max);

        if let Some(ref mut stream) = self.stream {
            let end = self.wr + max;
            loop {
                match stream.read(&mut self.inp[self.wr..end]) {
                    Ok(0) => {
                        self.inp_eof = true;
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

    /// Transfer outgoing data to the `upstream` TcpStreamBuf, and
    /// pull incoming data down from the `upstream` TcpStreamBuf.
    /// Also passes through EOF flags both ways.
    pub fn exchange(&mut self, upstream: &mut Self) {
        // Output
        if !self.out.is_empty() {
            upstream.out.append(&mut self.out);
        }
        if self.out_eof {
            upstream.out_eof = true;
        }

        // Input
        let read_len = upstream.wr - upstream.rd;
        if read_len > 0 {
            self.inp_makespace(read_len);
            self.inp[self.wr..self.wr + read_len]
                .copy_from_slice(&upstream.inp[upstream.rd..upstream.wr]);
            self.wr += read_len;
            upstream.rd = 0;
            upstream.wr = 0;
        }
        if upstream.inp_eof {
            self.inp_eof = true;
        }
    }
}

impl std::io::Read for TcpStreamBuf {
    /// Read data from the `inp` buffer, advancing the `rd` offset.
    /// If there is no data available in `inp`, returns `Ok(0)` if the
    /// EOF has been reached, otherwise
    /// `Err(ErrorKind::WouldBlock.into())`
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.wr == self.rd {
            if self.inp_eof {
                Ok(0)
            } else {
                Err(ErrorKind::WouldBlock.into())
            }
        } else {
            let len = buf.len().min(self.wr - self.rd);
            buf[..len].copy_from_slice(&self.inp[self.rd..self.rd + len]);
            self.rd += len;
            Ok(len)
        }
    }
}

impl std::io::Write for TcpStreamBuf {
    /// Write data into the `out` buffer
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let len = buf.len();
        self.out.extend_from_slice(buf);
        Ok(len)
    }

    /// Flush does nothing because we consider the end-target of the
    /// write to be the `TcpStreamBuf::out` buffer
    fn flush(&mut self) -> Result<()> {
        Ok(())
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
