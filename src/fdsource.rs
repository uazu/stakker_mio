use crate::mio::event::Source;
use crate::mio::unix::SourceFd;
use crate::mio::{Interest, Registry, Token};
use std::io::Result;
use std::os::unix::io::RawFd;

/// `Evented` instance to handle an arbitrary UNIX file descriptor
pub struct FdSource {
    pub fd: RawFd,
}

impl FdSource {
    pub fn new(fd: RawFd) -> Self {
        Self { fd }
    }
}

impl Source for FdSource {
    fn register(&mut self, poll: &Registry, token: Token, interest: Interest) -> Result<()> {
        SourceFd(&self.fd).register(poll, token, interest)
    }

    fn reregister(&mut self, poll: &Registry, token: Token, interest: Interest) -> Result<()> {
        SourceFd(&self.fd).reregister(poll, token, interest)
    }

    fn deregister(&mut self, poll: &Registry) -> Result<()> {
        SourceFd(&self.fd).deregister(poll)
    }
}
