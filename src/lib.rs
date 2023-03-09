//! Integrates **mio** into **Stakker**.
//!
//! [`MioPoll`] is the main type.  It handles polling and converting
//! mio events into **Stakker** forward calls.  It offers priority
//! levels to allow some events to take priority over others.  In
//! addition token cleanup is handled through drop handlers.
//!
//! [`TcpStreamBuf`] makes it easier to do buffering for a
//! [`mio::net::TcpStream`].
//!
//! [`FdSource`] wraps an arbitrary UNIX file descriptor for use with
//! [`MioPoll`].
//!
//! All calls retry on `ErrorKind::Interrupted` internally, so this
//! error doesn't have to be handled by the caller.  Retrying is the
//! most helpful behaviour in a non-blocking event loop.  In an app
//! using blocking I/O you might want a blocked call to be cut short
//! on a signal, but that case doesn't apply here.
//!
//! The mio version used by this library is re-exported as
//! `stakker_mio::mio`.  This should be used by applications in place
//! of importing mio directly, to guarantee they're using the same
//! version.
//!
//! [`FdSource`]: struct.FdSource.html
//! [`MioPoll`]: struct.MioPoll.html
//! [`TcpStreamBuf`]: struct.TcpStreamBuf.html
//! [`mio::net::TcpStream`]: ../mio/net/struct.TcpStream.html

#![deny(rust_2018_idioms)]
#![forbid(unsafe_code)]

/// Export the mio crate version that we are using, so that other
/// crates can be sure to use the same version.
pub use mio;

mod miopoll;
mod tcpstrbuf;
mod udpqueue;
mod udpserverqueue;

#[cfg(feature = "rustls")]
mod tls_rustls;

#[cfg(test)]
mod test;

pub use miopoll::{MioPoll, MioSource, Ready};
pub use tcpstrbuf::{ReadStatus, TcpStreamBuf};
pub use udpqueue::UdpQueue;
pub use udpserverqueue::UdpServerQueue;

#[cfg(feature = "rustls")]
pub use tls_rustls::{TlsEof, TlsError, TlsServerEngine};

#[cfg(unix)]
mod fdsource;
#[cfg(unix)]
pub use fdsource::FdSource;
