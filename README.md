# BoomNet &emsp; [![Build Status]][actions] [![Latest Version]][crates.io] [![Docs Badge]][docs]

[Build Status]: https://img.shields.io/endpoint.svg?url=https%3A%2F%2Factions-badge.atrox.dev%2Fhavefuntrading%2Fboomnet%2Fbadge%3Fref%3Dmain&style=flat&label=build&logo=none
[actions]: https://actions-badge.atrox.dev/havefuntrading/boomnet/goto?ref=main
[Latest Version]: https://img.shields.io/crates/v/boomnet.svg
[crates.io]: https://crates.io/crates/boomnet
[Docs Badge]: https://docs.rs/boomnet/badge.svg
[docs]: https://docs.rs/boomnet

Framework designed for writing low latency networking components. It's primary focus is
on writing TCP stream oriented client applications that operate using different protocols.

The framework is divided into several layers, where the next layer is built on top of the
previous one.

## Installing
Simply declare dependency on `boomnet` in your `Cargo.toml`.
```toml
[dependencies]
boomnet = { version = "0.0.2", features = ["full"]}
```

## Stream
Abstractions over TCP connection
* Must implement `Read` and `Write` traits
* Are non blocking
* TLS done with `rustls`
* Have ability to record and replay from network byte stream
* Can bind to specific network interface
* Used to implement TCP oriented client protocols (e.g. websocket, http, FIX)

Streams are fully generic and don't use dynamic dispatch and can be freely composed with
each other.

```rust
let stream: Recorded<TlsStream<MioStream>> = TcpStream::bind_and_connect(addr, self.net_iface, None)?
    .into_mio_stream()
    .into_tls_stream(self.url)
    .record();
```

The `stream` can then have protocol applied on top of it.
```rust
let ws: Websocket<Recorded<TlsStream<MioStream>>> = stream.into_websocket(self.url);
```

## Selector
Abstractions over OS specific mechanism (like `epoll`) that provide efficient way of checking
for readiness events on the monitored socket(s).
* At the moment supports both `mio` and `direct` (no-op) selectors 

Users don't usually deal with selectors directly as these are used internally by the IO service.

```rust
let mut io_service = MioSelector::new()?.into_io_service(IdleStrategy::Sleep(Duration::from_millis(1)));
```

## Service
The `IOService` uses `Selector` and manages `Endpoint` lifecycle. In addition, it provides auxiliary
services such as asynchronous DNS resolution.

`Endpoint` is the application level basic building block and is used to bind the connection and 
protocol to the business logic handler. The underlying connection lifecycle is managed by the `IOService`.

```rust
/// In this case we have a websocket endpoint.

impl TlsWebsocketEndpointWithContext<FeedContext> for TradeEndpoint {
    type Stream = MioStream;

    // the endpoint knows what protocol to use and how to create the underlying connection (stream)
    fn create_websocket(&self, addr: SocketAddr, ctx: &mut FeedContext) -> io::Result<TlsWebsocket<Self::Stream>> {

        let mut ws = TcpStream::bind_and_connect(addr, self.net_iface, None)?
            .into_mio_stream()
            .into_tls_websocket(self.url);
        
        // the endpoint can perform any other initialisation logic here
        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    // the IOService will poll the endpoint whenever there might be new data to process
    #[inline]
    fn poll(&self, ws: &mut TlsWebsocket<Self::Stream>, ctx: &mut FeedContext) -> io::Result<()> {
        while let Some(WebsocketFrame::Text(ts, fin, data)) = ws.receive_next()? {
            // business logic goes here
        }
        Ok(())
    }
}
```

`Endpoint` can be optionally linked to user provided `Context`. 

## Protocols

The framework will support a number of protocols out of the box, such as `websocket`, `http` and `fix`. Currently,
only websocket client is supported.

### Websocket

Websocket client protocol implementing [RFC 6455](https://datatracker.ietf.org/doc/html/rfc6455) specification.

* Works with any stream
* TCP batch aware timestamp (will provide same timestamp for each frame read in the same TCP batch)
* Will not block on partial frame
* Optimised for zero-copy read and write
* Optional masking of outbound frames
* Can be used standalone or together with `Selector` and `IOService`

## Features

The `full` feature will enable all features listed below. In situations where this is undesirable, individual
features can be enabled on demand.

* [mio](#mio)
* [tls](#mio)
* [ws](#ws)

### `mio`
Adds dependency on `mio` crate and enables `MioSelector` and `MioStream`

### `tls`
Adds dependency on `rustls` crate and enables `TlsStream` and more flexible `TlsReadyStream`

### `ws`
Adds support for `Websocket` protocol.
