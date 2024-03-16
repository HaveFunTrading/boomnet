# BoomNet &emsp; [![Build Status]][actions] [![Latest Version]][crates.io] [![Docs Badge]][docs]

[Build Status]: https://img.shields.io/endpoint.svg?url=https%3A%2F%2Factions-badge.atrox.dev%2Fhavefuntrading%2Fboomnet%2Fbadge%3Fref%3Dmain&style=flat&label=build&logo=none
[actions]: https://actions-badge.atrox.dev/havefuntrading/boomnet/goto?ref=main
[Latest Version]: https://img.shields.io/crates/v/boomnet.svg
[crates.io]: https://crates.io/crates/boomnet
[Docs Badge]: https://docs.rs/boomnet/badge.svg
[docs]: https://docs.rs/boomnet

Framework designed for building low latency networking components. It's primary focus is
on writing TCP stream oriented client applications that operate using different protocols.

## Installing
Simply declare dependency on `boomnet` in your `Cargo.toml` and select desired [features](#features).
```toml
[dependencies]
boomnet = { version = "0.0.2", features = ["full"]}
```

## Design Overview

The framework is divided into several layers, where the next layer is built on top of the
previous one.

### Stream
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

### Selector
Represents abstraction over OS specific mechanism (like `epoll`) that provides efficient way of checking
for readiness events on the monitored socket(s). At the moment both `mio` and `direct` (no-op) selectors
are supported. Users don't usually deal with selectors directly as these are used internally by the IO service.

```rust
let mut io_service = MioSelector::new()?.into_io_service(IdleStrategy::Sleep(Duration::from_millis(1)));
```

### Service
The `IOService` uses `Selector` and manages `Endpoint` lifecycle. In addition, it provides auxiliary
services such as asynchronous DNS resolution.

`Endpoint` is the application level building block and is used to bind the connection and protocol to the business
logic handler. The underlying connection lifecycle is managed by the `IOService`.

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

## Usage

Below example demonstrates how to use websocket client in order to consume messages from the Binance cryptocurrency
exchange. First, we need to define and implement our `Endpoint`. The framework provides `TlsWebsocketEndpoint` trait
that we can use.

```rust

struct TradeEndpoint {
    id: u32,
    url: &'static str,
    instrument: &'static str,
}

impl TradeEndpoint {
    pub fn new(id: u32, url: &'static str, instrument: &'static str) -> TradeEndpoint {
        Self { id, url, instrument, }
    }
}

impl TlsWebsocketEndpoint for TradeEndpoint {
    type Stream = MioStream;

    fn url(&self) -> &str {
        self.url
    }

    fn create_websocket(&self, addr: SocketAddr, ctx: &mut FeedContext) -> io::Result<TlsWebsocket<Self::Stream>> {
        
        // create secure websocket
        let mut ws = TcpStream::bind_and_connect(addr, None, None)?
            .into_mio_stream()
            .into_tls_websocket(self.url);

        // send subscription message
        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(ws)
    }

    #[inline]
    fn poll(&self, ws: &mut TlsWebsocket<Self::Stream>) -> io::Result<()> {
        // keep calling receive_next until no more frames in the current batch
        while let Some(WebsocketFrame::Text(ts, fin, data)) = ws.receive_next()? {
            // handle the message
            println!("{ts}: ({fin}) {}", String::from_utf8_lossy(data));
        }
        Ok(())
    }
}
```

All we need to do now is to register our endpoints with the `IOService` and keep polling the service in an event loop.
It is the responsibility of the service to manage the endpoint underlying connection and recreate it in case of any
disconnect.

```rust

fn main() -> anyhow::Result<()> {
    let mut io_service = MioSelector::new()?.into_io_service(IdleStrategy::Sleep(Duration::from_millis(1)));

    let endpoint_btc = TradeEndpoint::new(0, "wss://stream1.binance.com:443/ws", None, "btcusdt");
    let endpoint_eth = TradeEndpoint::new(1, "wss://stream2.binance.com:443/ws", None, "ethusdt");
    let endpoint_xrp = TradeEndpoint::new(2, "wss://stream3.binance.com:443/ws", None, "xrpusdt");

    io_service.register(endpoint_btc);
    io_service.register(endpoint_eth);
    io_service.register(endpoint_xrp);

    loop {
        // will never block
        io_service.poll()?;
    }
}
```

It is often required to expose some application state to the `Endpoint`. This can be achieved with user defined `Context`.

```rust
struct FeedContext {
    static_data: StaticData,
}

// use the market trait
impl Context for FeedContext {}
```

When implementing our `TradeEndpoint` we can use `TlsWebsocketEndpointWithContext` trait instead.
```rust
impl TlsWebsocketEndpointWithContext<FeedContext> for TradeEndpoint {
    type Stream = MioStream;

    fn url(&self) -> &str {
        self.url
    }

    fn create_websocket(&self, addr: SocketAddr, ctx: &mut FeedContext) -> io::Result<TlsWebsocket<Self::Stream>> {
        // we now have access to context
    }

    #[inline]
    fn poll(&self, ws: &mut TlsWebsocket<Self::Stream>, ctx: &mut FeedContext) -> io::Result<()> {
        // we now have access to context
    }
}
```

We will also need to create `IOService` with `Context`.

```rust
let mut context = FeedContext::new();

let mut io_service = MioSelector::new()?.into_io_service_with_context(IdleStrategy::Sleep(Duration::from_millis(1)), &mut context);
```

The `Context` must now be passed to the service `poll` method.
```rust
loop {
    io_service.poll(&mut context)?;
}
```

## Features

The `full` feature will enable all features listed below. In situations where this is undesirable, individual
features can be enabled on demand.

* [mio](#mio)
* [tls](#tls)
* [ws](#ws)

### `mio`
Adds dependency on `mio` crate and enables `MioSelector` and `MioStream`

### `tls`
Adds dependency on `rustls` crate and enables `TlsStream` and more flexible `TlsReadyStream`

### `ws`
Adds support for `Websocket` protocol.
