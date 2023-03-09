# Significant feature changes and additions

<!-- see keepachangelog.com for format ideas -->

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
