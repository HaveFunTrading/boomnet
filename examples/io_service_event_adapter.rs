//! Adapts lifecycle-oriented `IOService` events into an application event stream.
//!
//! This mirrors an exchange gateway: Boomnet owns connection lifecycle, while the adapter owns
//! WebSocket batching and translates active connections into domain-facing events.

use std::io;
use std::iter::Map;

use boomnet::service::select::mio::MioSelector;
use boomnet::service::{ActiveOutput, IOServiceEvent, IOServiceEvents, IntoIOService};
use boomnet::stream::mio::MioStream;
use boomnet::stream::tls::TlsStream;
use boomnet::ws::{BatchIter, Websocket, WebsocketFrame};

use crate::common::TradeEndpointFactory;

#[path = "common/mod.rs"]
mod common;

type TradeEndpoint = Websocket<TlsStream<MioStream>>;
type Frames<'a> = Map<
    BatchIter<'a, TlsStream<MioStream>>,
    fn(Result<WebsocketFrame, boomnet::ws::Error>) -> io::Result<WebsocketFrame>,
>;
type ActiveFrames<'a> = ActiveOutput<'a, Frames<'a>>;

struct ExchangeEvents<'a> {
    io_events: IOServiceEvents<'a, TradeEndpointFactory>,
    active_frames: Option<ActiveFrames<'a>>,
}

enum ExchangeEvent {
    Text { final_fragment: bool, body: &'static [u8] },
}

#[derive(Debug)]
enum ExchangeError {
    IO(io::Error),
}

impl<'a> ExchangeEvents<'a> {
    fn new(io_events: IOServiceEvents<'a, TradeEndpointFactory>) -> Self {
        Self {
            io_events,
            active_frames: None,
        }
    }
}

impl Iterator for ExchangeEvents<'_> {
    type Item = Result<ExchangeEvent, ExchangeError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(frame) = self.active_frames.as_mut().and_then(Iterator::next) {
                match frame {
                    Ok(WebsocketFrame::Text(final_fragment, body)) => {
                        return Some(Ok(ExchangeEvent::Text { final_fragment, body }));
                    }
                    Ok(_) => continue,
                    Err(error) => {
                        self.active_frames = None;
                        return Some(Err(ExchangeError::IO(error)));
                    }
                }
            }
            self.active_frames = None;

            match self.io_events.next()? {
                IOServiceEvent::Connected { handle } => {
                    log::info!("connected: {handle:?}");
                }
                IOServiceEvent::Disconnected { handle, reason } => {
                    log::warn!("disconnected: {handle:?}: {reason}");
                }
                IOServiceEvent::Active(active) => match active.try_with(read_frames) {
                    Ok(frames) => self.active_frames = Some(frames),
                    Err(error) => return Some(Err(ExchangeError::IO(error))),
                },
            }
        }
    }
}

fn read_frames(ws: &mut TradeEndpoint) -> io::Result<Frames<'_>> {
    ws.read_batch()
        .map(|batch| batch.into_iter().map(frame_to_io as fn(_) -> _))
        .map_err(io::Error::from)
}

fn frame_to_io(frame: Result<WebsocketFrame, boomnet::ws::Error>) -> io::Result<WebsocketFrame> {
    frame.map_err(io::Error::from)
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut io_service = MioSelector::new()?.into_io_service();
    io_service.register(TradeEndpointFactory::new("wss://stream.binance.com:443/ws", None, "btcusdt"))?;

    loop {
        for event in ExchangeEvents::new(io_service.poll(&mut ())?) {
            match event {
                Ok(ExchangeEvent::Text { final_fragment, body }) => {
                    println!("({final_fragment}) {}", String::from_utf8_lossy(body))
                }
                Err(ExchangeError::IO(error)) => log::warn!("exchange I/O error: {error}"),
            }
        }
    }
}
