use crate::mio::event::{Event, Source};
use crate::mio::{Events, Interest, Poll, Token, Waker};
use slab::Slab;
use stakker::{fwd_nop, Fwd, Stakker};
use std::cell::RefCell;
use std::io::{Error, ErrorKind, Result};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const WAKER_TOKEN: Token = Token(0);
const MAX_PRI: u32 = 10;

/// Wrapper for a mio `Source` instance
///
/// This is returned by the [`MioPoll::add`] method.  It takes care of
/// both unregistering the token and dropping the `Source` instance
/// when it is dropped.  It derefs to the contained `Source` instance,
/// so operations on the contained instance can be used directly.
///
/// [`MioPoll::add`]: struct.MioPoll.html#method.add
pub struct MioSource<S: Source> {
    token: Token,
    ctrl: Rc<RefCell<Control>>,
    source: S,
}

impl<S: Source> Drop for MioSource<S> {
    fn drop(&mut self) {
        let mut ctrl = self.ctrl.borrow_mut();
        if let Err(e) = ctrl.del(self.token, &mut self.source) {
            // TODO: Report the errors some other way, e.g. logged?
            ctrl.errors.push(e);
        }
    }
}

impl<S: Source> Deref for MioSource<S> {
    type Target = S;
    fn deref(&self) -> &Self::Target {
        &self.source
    }
}

impl<S: Source> DerefMut for MioSource<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.source
    }
}

/// Ref-counting wrapper around a mio `Poll` instance
///
/// After creation, pass cloned copies of this to all interested
/// parties.  A `MioPoll` reference is also available from the
/// associated **Stakker** instance using
/// `cx.anymap_get::<MioPoll>()`.
pub struct MioPoll {
    rc: Rc<RefCell<Control>>,
}

impl MioPoll {
    /// Create a new MioPoll instance wrapping the given mio `Poll`
    /// instance and mio `Events` queue (which the caller should size
    /// according to their requirements).  The waker priority should
    /// also be provided, in the range `0..=10`.  Sets up the
    /// **Stakker** instance to use `MioPoll` as the poll-waker, and
    /// puts a `MioPoll` clone into the **Stakker** anymap.
    pub fn new(stakker: &mut Stakker, poll: Poll, events: Events, waker_pri: u32) -> Result<Self> {
        let mut token_map = Slab::with_capacity(256);

        let waker_pri = waker_pri.min(MAX_PRI);
        let waker_token = Token(token_map.insert(Entry {
            pri: waker_pri,
            fwd: fwd_nop!(),
        }));
        assert_eq!(waker_token, WAKER_TOKEN);
        let waker = Arc::new(Waker::new(poll.registry(), WAKER_TOKEN)?);
        let waker2 = waker.clone();

        let mut ctrl = Control {
            poll,
            token_map,
            queues: Default::default(),
            max_pri: waker_pri,
            events,
            errors: Vec::new(),
            waker,
        };

        let deferrer = stakker.deferrer();
        ctrl.set_wake_fwd(Fwd::new(move |_| deferrer.defer(|s| s.poll_wake())));

        let miopoll = Self {
            rc: Rc::new(RefCell::new(ctrl)),
        };

        stakker.anymap_set(miopoll.clone());
        stakker.set_poll_waker(move || {
            if let Err(e) = waker2.wake() {
                panic!("Inter-thread poll waker failed: {}", e);
            }
        });

        Ok(miopoll)
    }

    /// Register a mio `Source` object with the poll instance.
    /// Returns a [`MioSource`] which takes care of cleaning up the
    /// token and handler when it is dropped.
    ///
    /// This uses edge-triggering: whenever one of the Interest flags
    /// included in `ready` changes state, the given `Fwd` instance
    /// will be invoked with the new `Ready` value.  The contract with
    /// the handler is that there may be spurious calls to it, so it
    /// must be ready for that.
    ///
    /// `pri` gives a priority level: `0..=10`.  If handlers are
    /// registered at different priority levels, then higher priority
    /// events get handled before lower priority events.  Under
    /// constant very heavy load, lower priority events might be
    /// delayed indefinitely.
    ///
    /// [`MioSource`]: struct.MioSource.html
    pub fn add<S: Source>(
        &self,
        mut source: S,
        ready: Interest,
        pri: u32,
        fwd: Fwd<Ready>,
    ) -> Result<MioSource<S>> {
        let token = self.rc.borrow_mut().add(&mut source, ready, pri, fwd)?;
        Ok(MioSource {
            token,
            ctrl: self.rc.clone(),
            source,
        })
    }

    /// Poll for new events and queue all the events of the highest
    /// available priority level.  Events of lower priority levels are
    /// queued internally to be used on a future call to this method.
    ///
    /// So the expected pattern is that highest-priority handlers get
    /// run, and when all the resulting processing has completed in
    /// **Stakker**, then the main loop polls again, and if more
    /// high-priority events have occurred, then those too will get
    /// processed.  Lower-priority handlers will only get a chance to
    /// run when nothing higher-priority needs handling.
    ///
    /// On success returns `Ok(true)` if an event was processed, or
    /// `Ok(false)` if there were no new events.
    pub fn poll(&self, max_delay: Duration) -> Result<bool> {
        self.rc.borrow_mut().poll(max_delay)
    }

    /// Set the handler for "wake" events.  There can only be one
    /// handler for "wake" events, so setting it here drops the
    /// previous handler.  Don't call this unless you wish to override
    /// the default wake handling which calls
    /// [`stakker::Stakker::poll_wake`].
    ///
    /// [`stakker::Stakker::poll_wake`]: ../stakker/struct.Stakker.html#method.poll_wake
    pub fn set_wake_fwd(&mut self, fwd: Fwd<Ready>) {
        self.rc.borrow_mut().set_wake_fwd(fwd);
    }

    /// Get a cloned reference to the waker for this `MioPoll`
    /// instance.  This can be passed to other threads, which can call
    /// `wake()` on it to cause the wake handler to be run in the main
    /// polling thread.
    pub fn waker(&mut self) -> Arc<Waker> {
        self.rc.borrow_mut().waker.clone()
    }
}

impl Clone for MioPoll {
    fn clone(&self) -> Self {
        Self {
            rc: self.rc.clone(),
        }
    }
}

struct QueueEvent {
    token: usize,
    ready: Ready,
}

struct Entry {
    pri: u32,
    fwd: Fwd<Ready>,
}

struct Control {
    token_map: Slab<Entry>,
    poll: Poll,
    // Highest priority in use goes on a fast path so we need queues
    // only for 0..=9
    queues: [Vec<QueueEvent>; MAX_PRI as usize],
    max_pri: u32,
    events: Events,
    errors: Vec<Error>,
    waker: Arc<Waker>,
}

impl Control {
    #[inline]
    fn del(&mut self, token: Token, handle: &mut impl Source) -> Result<()> {
        let rv = self.poll.registry().deregister(handle);
        if self.token_map.contains(token.into()) {
            self.token_map.remove(token.into());
            return rv;
        }
        rv.and(Err(Error::from(ErrorKind::NotFound)))
    }

    #[inline]
    fn add(
        &mut self,
        handle: &mut impl Source,
        ready: Interest,
        pri: u32,
        fwd: Fwd<Ready>,
    ) -> Result<Token> {
        let pri = pri.min(MAX_PRI);
        self.max_pri = self.max_pri.max(pri);
        let token = Token(self.token_map.insert(Entry { pri, fwd }));
        self.poll.registry().register(handle, token, ready)?;
        Ok(token)
    }

    fn poll(&mut self, max_delay: Duration) -> Result<bool> {
        self.poll.poll(&mut self.events, Some(max_delay))?;
        let mut done = false;
        for ev in &self.events {
            let token = ev.token().into();
            if let Some(ref mut entry) = self.token_map.get_mut(token) {
                // Fast-path for highest priority level present in
                // registrations, so if user uses only one priority level,
                // there is no queuing necessary here.
                let ready = Ready::new(ev);
                if entry.pri == self.max_pri {
                    done = true;
                    entry.fwd.fwd(ready);
                } else {
                    self.queues[entry.pri as usize].push(QueueEvent { token, ready });
                }
            }
        }
        self.events.clear();
        if !done {
            for qu in self.queues.iter_mut().rev() {
                if !qu.is_empty() {
                    for qev in qu.drain(..) {
                        if let Some(ref mut entry) = self.token_map.get_mut(qev.token) {
                            done = true;
                            entry.fwd.fwd(qev.ready);
                        }
                    }
                    if done {
                        break;
                    }
                }
            }
        }
        Ok(done)
    }

    fn set_wake_fwd(&mut self, fwd: Fwd<Ready>) {
        self.token_map[WAKER_TOKEN.0].fwd = fwd;
    }
}

/// Readiness information from `mio`
///
/// See [`mio::event::Event`] for an explanation of what these flags
/// mean.
///
/// [`mio::event::Event`]: ../mio/event/struct.Event.html
pub struct Ready(u16);

const READY_RD: u16 = 1;
const READY_WR: u16 = 2;
const READY_ERROR: u16 = 4;
const READY_RD_CLOSED: u16 = 8;
const READY_WR_CLOSED: u16 = 16;
const READY_PRIORITY: u16 = 32;
const READY_AIO: u16 = 64;
const READY_LIO: u16 = 128;

impl Ready {
    fn new(ev: &Event) -> Self {
        macro_rules! test {
            ($test:expr, $val:expr) => {
                (if $test { $val } else { 0 })
            };
        }
        // TODO: Ask 'mio' maintainers to add #[inline] if these
        // aren't getting inlined
        let val = test!(ev.is_readable(), READY_RD)
            + test!(ev.is_writable(), READY_WR)
            + test!(ev.is_error(), READY_ERROR)
            + test!(ev.is_read_closed(), READY_RD_CLOSED)
            + test!(ev.is_write_closed(), READY_WR_CLOSED)
            + test!(ev.is_priority(), READY_PRIORITY)
            + test!(ev.is_aio(), READY_AIO)
            + test!(ev.is_lio(), READY_LIO);
        Self(val)
    }
    #[inline]
    pub fn is_readable(&self) -> bool {
        0 != (READY_RD & self.0)
    }
    #[inline]
    pub fn is_writable(&self) -> bool {
        0 != (READY_WR & self.0)
    }
    #[inline]
    pub fn is_error(&self) -> bool {
        0 != (READY_ERROR & self.0)
    }
    #[inline]
    pub fn is_read_closed(&self) -> bool {
        0 != (READY_RD_CLOSED & self.0)
    }
    #[inline]
    pub fn is_write_closed(&self) -> bool {
        0 != (READY_WR_CLOSED & self.0)
    }
    #[inline]
    pub fn is_priority(&self) -> bool {
        0 != (READY_PRIORITY & self.0)
    }
    #[inline]
    pub fn is_aio(&self) -> bool {
        0 != (READY_AIO & self.0)
    }
    #[inline]
    pub fn is_lio(&self) -> bool {
        0 != (READY_LIO & self.0)
    }
}
