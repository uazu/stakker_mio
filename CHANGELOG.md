# Significant feature changes and additions

<!-- see keepachangelog.com for format ideas -->

## 0.3.0 (2026-01-14)

Update to `mio` 1.1, which requires MSRV 1.71

### Breaking change

- `MioPoll::poll` now returns two values on success: `(activity,
  io_pending)`.  This is because the example main loop I/O code in
  `stakker` crate did not behave correctly when more than one I/O
  priority level was triggered within the same poll.  The example code
  would do a poll-sleep even though there were more I/O events still
  to process, meaning that those events would be processed late.  The
  `MioPoll::poll` call was behaving correctly (as described), and its
  behaviour has not been changed, but to make sure people get their
  main loops corrected and to simplify the fix, the signature was
  changed to return the `io_pending` value.

  A correct main loop now looks like this:

  ```
    let mut idle_pending = stakker.run(Instant::now(), false);
    let mut io_pending = false;
    let mut activity;
    const MAX_WAIT: Duration = Duration::from_secs(60);
    while stakker.not_shutdown() {
        let maxdur = stakker.next_wait_max(Instant::now(), MAX_WAIT, idle_pending || io_pending);
        (activity, io_pending) = miopoll.poll(maxdur)?;
        idle_pending = stakker.run(Instant::now(), !activity);
    }
  ```

### Fixed

- UDP queues flush operation didn't flush all packets


## 0.2.5 (2023-03-14)

### Added

- `TlsServerEngine` to handle TLS wrapping on any server-side TCP
  stream.  Uses `rustls` and requires that the `rustls` feature is
  enabled.

- `TcpStreamBuf` improvements:
  - `Read` and `Write` implementations
  - Input EOF flag
  - Ability to pause flush (for Windows before "write ready")
  - Forward data from one `TcpStreamBuf` to another (`exchange`)

## 0.2.4 (2022-09-06)

Update to `mio` version 0.8.*

## 0.2.3 (2021-11-01)

### Added

- `UdpServerQueue`: similar to `UdpQueue` but allows packets to be
  sent and received from any address

## 0.2.2 (2021-05-14)

### Documentation

- TCP `echo_server` example

## 0.2.1 (2021-03-17)

### Added

- `UdpQueue` for queuing for connected UDP sockets

## 0.2.0 (2020-11-08)

First version for Stakker 0.2

<!-- Local Variables: -->
<!-- mode: markdown -->
<!-- End: -->
