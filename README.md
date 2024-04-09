<h1 align="left"><img width="500" src="https://raw.githubusercontent.com/havefuntrading/boomnet/564a67d22e841eed48aa8a1db9cf7c7847ec281d/docs/thumbnail.png"/></h1>

[![Build Status]][actions] [![Latest Version]][crates.io] [![Docs Badge]][docs] [![License Badge]][license]

[Build Status]: https://img.shields.io/endpoint.svg?url=https%3A%2F%2Factions-badge.atrox.dev%2Fhavefuntrading%2Fboomnet%2Fbadge%3Fref%3Dmain&style=flat&label=build&logo=none
[actions]: https://actions-badge.atrox.dev/havefuntrading/boomnet/goto?ref=main
[Latest Version]: https://img.shields.io/crates/v/boomnet.svg
[crates.io]: https://crates.io/crates/boomnet
[Docs Badge]: https://docs.rs/boomnet/badge.svg
[docs]: https://docs.rs/boomnet
[License Badge]: https://img.shields.io/badge/License-MIT-blue.svg
[license]: LICENSE

## Overview
BoomNet is a high-performance framework targeting development of low-latency network applications,
particularly focusing on TCP stream-oriented clients that utilise various protocols.

## Installation
Simply declare dependency on `boomnet` in your `Cargo.toml` and select desired [features](#features).
```toml
[dependencies]
boomnet = { version = "0.0.20", features = ["full"]}
```

## Design Principles

The framework is structured into multiple layers, with each subsequent layer building upon its predecessor,
enhancing functionality and abstraction.

### Stream
The first layer defines `stream` as abstraction over TCP connection, adhering to the following characteristics.

* Must implement `Read` and `Write` traits for I/O operations.
* Operates in a non-blocking manner.
* Integrates with TLS using `rustls`.
* Supports recording and replay of network byte streams.
* Allows binding to specific network interface.
* Facilitates implementation of TCP oriented client protocols such as WebSocket, HTTP, and FIX.

Streams are designed to be fully generic, avoiding dynamic dispatch, and can be composed in flexible way.

```rust
let stream: RecordedStream<TlsStream<TcpStream>> = TcpStream::bind_and_connect(addr, self.net_iface, None)?
    .into_tls_stream(self.url)
    .into_recorded_stream("plain");
```

Different protocols can then be applied on top of a stream.
```rust
let ws: Websocket<RecordedStream<TlsStream<TcpStream>>> = stream.into_websocket(self.url);
```

### Selector
`Selector` provides abstraction over OS specific mechanisms (like `epoll`) for efficiently monitoring socket readiness events.
Though primarily utilised internally, selectors are crucial for the `IOService` functionality, currently offering both
`mio` and `direct` (no-op) implementations.

```rust
let mut io_service = MioSelector::new()?.into_io_service(IdleStrategy::Sleep(Duration::from_millis(1)));
```

### Service
The last layer manages lifecycle of endpoints and provides auxiliary services (such as asynchronous DNS resolution and
auto disconnect) through the `IOService`.

`Endpoint` serves as low level construct for application logic. `IOService` oversees the connection lifecycle within endpoints.

## Protocols
The aim is to support a variety of protocols, including WebSocket, HTTP, and FIX, with WebSocket client
functionality currently available.

### Websocket
The websocket client protocol complies with the [RFC 6455](https://datatracker.ietf.org/doc/html/rfc6455) specification,
offering the following features.

* Compatibility with any stream.
* TCP batch-aware timestamps for frames read in the same batch.
* Not blocking on partial frame(s).
* Designed for zero-copy read and write.
* Optional masking of outbound frames.
* Standalone usage or in conjunction with `Selector` and `IOService`.

## Example Usage

The repository contains comprehensive list of [examples](https://github.com/HaveFunTrading/boomnet/tree/main/examples).

The following example illustrates how to use multiple websocket connections with `IOService` in order to consume messages from the Binance cryptocurrency
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

    // called by the IO service whenever a connection has to be established for this endpoint
    fn create_websocket(&self, addr: SocketAddr) -> io::Result<TlsWebsocket<Self::Stream>> {
        
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
    fn poll(&mut self, ws: &mut TlsWebsocket<Self::Stream>) -> io::Result<()> {
        // keep calling receive_next until no more frames in the current batch
        while let Some(WebsocketFrame::Text(ts, fin, data)) = ws.receive_next()? {
            // handle the message
            println!("[{}] {ts}: ({fin}) {}", self.id, String::from_utf8_lossy(data));
        }
        Ok(())
    }
}
```

After defining the endpoint, it is registered with the `IOService` and polled within an event loop. The service handles
`Endpoint` connection management and reconnection in case of disconnection.

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

It is often required to expose shared state to the `Endpoint`. This can be achieved with user defined `Context`.

```rust
struct FeedContext {
    static_data: StaticData,
}

// use the marker trait
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
    fn poll(&mut self, ws: &mut TlsWebsocket<Self::Stream>, ctx: &mut FeedContext) -> io::Result<()> {
        // we now have access to context
    }
}
```

We will also need to create `IOService` that is `Context` aware.

```rust
let mut context = FeedContext::new(static_data);
let mut io_service = MioSelector::new()?.into_io_service_with_context(IdleStrategy::Sleep(Duration::from_millis(1)), &mut context);
```

The `Context` must now be passed to the service `poll` method.
```rust
loop {
    io_service.poll(&mut context)?;
}
```

## Features
BoomNet feature set is modular, allowing for tailored functionality based on project needs. The `full` feature enables
all available features, while individual components can be enabled as needed.

* [mio](#mio)
* [tls](#tls)
* [ws](#ws)

### `mio`
Adds dependency on `mio` crate and enables `MioSelector` and `MioStream`.

### `tls`
Adds dependency on `rustls` crate and enables `TlsStream` and more flexible `TlsReadyStream`.

### `ws`
Adds support for `Websocket` protocol.
