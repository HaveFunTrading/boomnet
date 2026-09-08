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
boomnet = { version = "0.0.89", features = ["rustls-webpki", "ws", "mio"]}
```

## Design Principles

The framework is structured into multiple layers, with each subsequent layer building upon its predecessor,
enhancing functionality and abstraction.

### Stream
The first layer defines `stream` as abstraction over TCP connection, adhering to the following characteristics.

* Must implement `Read` and `Write` traits for I/O operations.
* Operates in a non-blocking manner.
* Integrates with TLS using `rustls` or `openssl`.
* Supports recording and replay of network byte streams.
* Allows binding to specific network interface.
* Facilitates implementation of TCP oriented client protocols such as WebSocket, HTTP, and FIX.

Streams are designed to be fully generic, avoiding dynamic dispatch, and can be composed in flexible way.

```rust
let stream: RecordedStream<TlsStream<TcpStream>> = TcpStream::try_from((host, port))?
    .into_tls_stream()?
    .into_default_recorded_stream();
```

Different protocols can then be applied on top of a stream in order to create a client.
```rust
let ws: Websocket<RecordedStream<TlsStream<TcpStream>>> = stream.into_websocket("/ws");
```

### Selector
`Selector` provides abstraction over OS specific mechanisms (like `epoll`) for efficiently monitoring socket readiness events.
Though primarily utilised internally, selectors are crucial for the `IOService` functionality, currently offering both
`mio` and `direct` (no-op) implementations.

```rust
let mut io_service = MioSelector::new()?.into_io_service();
```

### Service
The last layer manages lifecycle of endpoints and provides auxiliary services (such as asynchronous DNS resolution and
auto disconnect) through the `IOService`.

`EndpointFactory` holds connection configuration and lifecycle policy. `IOService` retains each registered
factory across reconnects and exposes its live endpoint through `ActiveEndpoint` for application I/O.

## Protocols
The aim is to support a variety of protocols, including WebSocket, HTTP, and FIX.

### Websocket
The websocket client protocol complies with the [RFC 6455](https://datatracker.ietf.org/doc/html/rfc6455) specification,
offering the following features.

* Compatibility with any stream.
* TCP batch-aware frame processing.
* Not blocking on partial frame(s).
* No memory allocations (except to initialise buffers)
* Designed for zero-copy read and write.
* Optional masking of outbound frames.
* Standalone usage or in conjunction with `IOService`.

### Http
Provides http 1.1 client that is compatible with any non-blocking stream and does perform memory allocations. 

## Example Usage

The repository contains comprehensive list of [examples](https://github.com/HaveFunTrading/boomnet/tree/main/examples).

The following example illustrates how to use multiple websocket connections with `IOService` in order to consume messages from the Binance cryptocurrency
exchange. First, we define an `EndpointFactory` that creates a WebSocket over TLS.

```rust

struct TradeEndpointFactory {
    connection_info: ConnectionInfo,
    ws_endpoint: String,
    instrument: &'static str,
}

impl TradeEndpointFactory {
    pub fn new(url: &'static str, instrument: &'static str) -> TradeEndpointFactory {
        let (connection_info, ws_endpoint, _) = boomnet::ws::util::parse_url(url).unwrap();
        Self { connection_info, ws_endpoint, instrument, }
    }
}

impl ConnectionInfoProvider for TradeEndpointFactory {
    fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl EndpointFactory for TradeEndpointFactory {
    type Context = ();
    type Endpoint = Websocket<TlsStream<MioStream>>;

    // called by the IO service whenever a connection has to be established for this endpoint
    fn create_endpoint(&mut self, addr: SocketAddr, _ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>> {

        let mut ws = TcpStream::try_from((&self.connection_info, addr))?
            .into_mio_stream()
            .into_tls_websocket(&self.ws_endpoint)?;

        // send subscription message
        ws.send_text(
            true,
            Some(format!(r#"{{"method":"SUBSCRIBE","params":["{}@trade"],"id":1}}"#, self.instrument).as_bytes()),
        )?;

        Ok(Some(ws))
    }
}
```

After defining the factory, it is registered with the `IOService` and polled within an event loop. The service handles
connection lifecycle and exposes every active endpoint through an `ActiveEndpoint` guard. I/O performed with `try_with`
automatically starts the endpoint's reconnection lifecycle if it fails. The handle returned by `register`
identifies the registration and stays the same when the factory creates a replacement endpoint.

```rust

fn main() -> anyhow::Result<()> {
    let mut io_service = MioSelector::new()?.into_io_service();

    let factory_btc = TradeEndpointFactory::new("wss://stream1.binance.com:443/ws", "btcusdt");
    let factory_eth = TradeEndpointFactory::new("wss://stream2.binance.com:443/ws", "ethusdt");
    let factory_xrp = TradeEndpointFactory::new("wss://stream3.binance.com:443/ws", "xrpusdt");

    io_service.register(factory_btc)?;
    io_service.register(factory_eth)?;
    io_service.register(factory_xrp)?;

    loop {
        // will never block
        for event in io_service.poll(&mut ())? {
            if let IOServiceEvent::Active(active) = event {
                let handle = active.handle();
                active.try_with(|ws| {
                    for frame in ws.read_batch()? {
                        if let WebsocketFrame::Text(fin, data) = frame? {
                            println!("[{handle:?}] ({fin}) {}", String::from_utf8_lossy(data));
                        }
                    }
                    Ok(())
                })?;
            }
        }
    }
}
```

Each factory declares its lifecycle context with `type Context`. Use `()` when callbacks need
no shared state, as above, and pass `&mut ()` to `poll`. To use application state, set the
associated type on the same `EndpointFactory` trait:

```rust
#[derive(Default)]
struct FeedContext {
    connection_attempts: usize,
    frames_processed: usize,
}

impl EndpointFactory for TradeEndpointFactory {
    type Endpoint = Websocket<TlsStream<MioStream>>;
    type Context = FeedContext;

    fn create_endpoint(&mut self, addr: SocketAddr, ctx: &mut Self::Context) -> io::Result<Option<Self::Endpoint>> {
        ctx.connection_attempts += 1;
        // Create and subscribe the WebSocket as above.
        // ...
    }
}
```

Service construction is the same for every factory. Context stays owned by the caller and is borrowed
only during lifecycle callbacks. The returned iterator borrows the service, so application
processing can immediately use context too:

```rust
let mut context = FeedContext::default();
let mut io_service = MioSelector::new()?.into_io_service();
io_service.register(TradeEndpointFactory::new("wss://stream.binance.com:443/ws", "btcusdt"))?;

loop {
    for event in io_service.poll(&mut context)? {
        if let IOServiceEvent::Active(active) = event {
            active.try_with(|ws| {
                for frame in ws.read_batch()? {
                    let _frame = frame?;
                    context.frames_processed += 1;
                }
                Ok(())
            })?;
        }
    }
}
```

`dispatch` closures can also capture application state directly; there is no separate context
argument. Explicit event iterator types only need the factory type: `IOServiceEvents<'a, TradeEndpointFactory>`.
See [the context example](examples/io_service_with_context.rs) for a complete implementation that
shares lifecycle counters across endpoints.

## Features
The framework feature set is modular, allowing for tailored functionality based on project needs.

* [mio](#mio)
* [rustls-native](#rustls-native)
* [rustls-webpki](#rustls-webpki)
* [openssl](#openssl)
* [ktls](#ktls)
* [ws](#ws)
* [http](#http)

### `mio`
Adds dependency on `mio` crate and enables `MioSelector` and `MioStream`.

### `rustls-native`
Adds dependency on `rustls` crate with `rustls-native-certs` and enables `TlsStream` as well as more flexible `TlsReadyStream`.

### `rustls-webpki`
Adds dependency on `rustls` crate with `webpki-roots` and enables `TlsStream` as well as more flexible `TlsReadyStream`.

### `openssl`
Adds dependency on `openssl` crate and enables `TlsStream` as well as more flexible `TlsReadyStream`.

### `ktls`
Activates `openssl` feature and enables `KtlsStream` that offloads TLS to the kernel (KTLS).

### `ws`
Adds support for `Websocket` protocol.

### `http`
Adds support for `Http1.1` protocol.
