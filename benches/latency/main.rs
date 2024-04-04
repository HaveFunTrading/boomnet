use std::net::TcpStream;

use ::tungstenite::{connect, Message};
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use url::Url;

use ::boomnet::stream::buffer::IntoBufferedStream;
use ::boomnet::ws::IntoWebsocket;

mod server;

const MSG: &str = unsafe { std::str::from_utf8_unchecked(&[90u8; 256]) };

fn boomnet_rtt_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("boomnet");
    group.throughput(Throughput::Bytes(MSG.len() as u64));

    // run serer in the background
    server::start_on_thread(9002);

    // setup client
    let stream = TcpStream::connect("127.0.0.1:9002").unwrap();
    stream.set_nonblocking(true).unwrap();
    stream.set_nodelay(true).unwrap();
    let mut ws = stream
        .into_default_buffered_stream()
        .into_websocket("ws://127.0.0.1:9002");

    group.bench_function("boomnet_rtt", |b| {
        b.iter(|| {
            ws.send_text(true, Some(MSG.as_bytes())).unwrap();
            loop {
                if ws.receive_next().unwrap().is_some() {
                    break;
                }
            }
        })
    });

    group.finish();
}

fn tungstenite_rtt_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("tungstenite");
    group.throughput(Throughput::Bytes(MSG.len() as u64));

    // run serer in the background
    server::start_on_thread(9001);

    // setup client
    let (mut ws, _) = connect(Url::parse("ws://127.0.0.1:9001").unwrap()).unwrap();

    group.bench_function("tungstenite_rtt", |b| {
        b.iter(|| {
            ws.write(Message::Text(MSG.to_owned())).unwrap();
            ws.flush().unwrap();
            if let Message::Text(data) = ws.read().unwrap() {
                black_box(data);
            }
        })
    });

    group.finish();
}

criterion_group!(benches, boomnet_rtt_benchmark, tungstenite_rtt_benchmark);
criterion_main!(benches);
