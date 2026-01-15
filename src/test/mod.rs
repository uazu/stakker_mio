mod udp;
mod udpserver;

use crate::{
    mio::{Events, Poll},
    MioPoll,
};
use stakker::Stakker;
use std::time::{Duration, Instant};

/// Really we want to enable Stakker "multi-thread" feature, but only
/// in dev-dependencies.  For this to not affect crate users it seems
/// like this requires Rust 2021 edition.  So work around it for now
/// by running all tests under a single `#[test]`
#[test]
fn all_tests() {
    println!("=== udp::test_udp");
    udp::test_udp();
    println!("=== udpserver::test_udpserver");
    udpserver::test_udpserver();
}

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
        10, // Wake priority
    )
    .expect("MioPoll::new failed");
    stakker
}

/// Run event loop.  Don't need timers or idle queue for this test,
/// but run a full event loop anyway in case someone copies this code.
fn run(s: &mut Stakker) -> std::io::Result<()> {
    let miopoll = s.anymap_get::<MioPoll>();

    let mut idle_pending = s.run(Instant::now(), false);
    let mut io_pending = false;
    let mut activity;
    const MAX_WAIT: Duration = Duration::from_secs(60);
    while s.not_shutdown() {
        let maxdur = s.next_wait_max(Instant::now(), MAX_WAIT, idle_pending || io_pending);
        (activity, io_pending) = miopoll.poll(maxdur)?;
        idle_pending = s.run(Instant::now(), !activity);
    }

    Ok(())
}
