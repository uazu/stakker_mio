use crate::TcpStreamBuf;
use rustls::{ServerConfig, ServerConnection};
use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;

/// TLS end-of-file state
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TlsEof {
    /// Clean EOF
    Close,
    /// Unclean EOF
    Abort,
}

/// Error in TLS processing
#[derive(Debug)]
pub struct TlsError(String);

impl std::error::Error for TlsError {}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Wraps `rustls` to make it easier to use with [`TcpStreamBuf`]
///
/// If TLS is not configured then just passes data through unchanged.
pub struct TlsServerEngine {
    sc: Option<ServerConnection>,
    sent_out_eof: bool,
    inp_eof: Option<TlsEof>,
}

impl TlsServerEngine {
    /// Create a new TLS engine using the given `rustls`
    /// configuration, or set it up to just pass data straight through
    /// if there is no configuration provided
    pub fn new(config: Option<Arc<ServerConfig>>) -> Result<Self, rustls::Error> {
        let sc = if let Some(conf) = config {
            Some(ServerConnection::new(conf)?)
        } else {
            None
        };

        Ok(Self {
            sc,
            sent_out_eof: false,
            inp_eof: None,
        })
    }

    /// Process as much data as possible, moving data between `ext`
    /// (external TCP connection dealing with TLS protocol data) and
    /// `int` (internal buffers dealing with plain-text).  This will
    /// flush `ext` if there is new data output to the network.  So
    /// the caller must set `ext.pause_writes(true)` until the first
    /// "ready for write" indication is received.
    ///
    /// If TLS is disabled, just passes data straight through.
    ///
    /// When the output EOF flag is set on `int`, the outgoing side of
    /// the connection is closed at the TLS level and the flag is
    /// passed through to `ext` to close the TCP connection.
    ///
    /// Returns the input EOF state, or `None` if the input side has
    /// not reached EOF yet.
    pub fn process(
        &mut self,
        ext: &mut TcpStreamBuf,
        int: &mut TcpStreamBuf,
    ) -> Result<Option<TlsEof>, TlsError> {
        if let Some(ref mut sc) = self.sc {
            // Process TLS stream both ways
            loop {
                if sc.wants_write() {
                    // Not expecting any error from this
                    sc.write_tls(ext).map_err(|e| {
                        TlsError(format!(
                            "Unexpected error from ServerConnection::write_tls: {e}"
                        ))
                    })?;
                    if self.sent_out_eof && !sc.wants_write() {
                        ext.out_eof = true;
                    }
                    ext.flush()
                        .map_err(|e| TlsError(format!("Failed to flush TCP: {e}")))?;
                    continue;
                }
                if !int.out.is_empty() {
                    // Not expecting any error
                    let len = sc.writer().write(&int.out[..]).map_err(|e| {
                        TlsError(format!(
                            "Unexpected error from ServerConnection::writer.write: {e}"
                        ))
                    })?;
                    int.out.drain(..len);
                    continue;
                }
                if int.out_eof && int.out.is_empty() && !self.sent_out_eof {
                    sc.send_close_notify();
                    self.sent_out_eof = true;
                    continue;
                }
                if sc.wants_read() && ext.rd != ext.wr {
                    // We don't expect any error from this.  The
                    // `TcpStreamBuf` `Read` implementation doesn't return
                    // an error if there are bytes.  The call may return
                    // an error if its buffer is full, but we only call it
                    // when it wants more data.
                    sc.read_tls(ext).map_err(|e| {
                        TlsError(format!(
                            "Unexpected failure from ServerConnection::read_tls: {e}"
                        ))
                    })?;

                    let state = sc
                        .process_new_packets()
                        .map_err(|e| TlsError(format!("TLS stream error: {e}")))?;

                    let read_len = state.plaintext_bytes_to_read();
                    if read_len > 0 {
                        int.inp_makespace(read_len);
                        loop {
                            match sc.reader().read(&mut int.inp[int.wr..]) {
                                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                                Ok(0) => {
                                    self.inp_eof = Some(TlsEof::Close);
                                }
                                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                                    self.inp_eof = Some(TlsEof::Abort);
                                }
                                Ok(len) => int.wr += len,
                                Err(e) => return Err(TlsError(format!("TLS read error: {e}"))),
                            }
                        }
                    }
                    continue;
                }
                break;
            }
        } else {
            // Pass data through unchanged
            int.exchange(ext);
            if int.inp_eof {
                self.inp_eof = Some(TlsEof::Close);
            }
            ext.flush()
                .map_err(|e| TlsError(format!("Failed to flush TCP: {e}")))?;
        }
        Ok(self.inp_eof)
    }
}
