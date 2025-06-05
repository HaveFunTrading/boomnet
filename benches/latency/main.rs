use ::tungstenite::{Message, connect};
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use tungstenite::Utf8Bytes;

use crate::endpoint::{TestContext, TestEndpoint};
use ::boomnet::stream::buffer::IntoBufferedStream;
use ::boomnet::ws::IntoWebsocket;
use boomnet::service::IntoIOServiceWithContext;
use boomnet::service::select::direct::DirectSelector;
use boomnet::stream::ConnectionInfo;

mod endpoint;
mod server;

const MSG: &str = unsafe { std::str::from_utf8_unchecked(&[90u8; 256]) };

fn boomnet_rtt_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("boomnet");
    group.throughput(Throughput::Bytes(MSG.len() as u64));

    // run server in the background
    server::start_on_thread(9002);

    // affinity
    core_affinity::set_for_current(core_affinity::CoreId { id: 8 });

    // setup client
    let mut ws = ConnectionInfo::new("127.0.0.1", 9002)
        .into_tcp_stream()
        .unwrap()
        .into_default_buffered_stream()
        .into_websocket("/");

    group.bench_function("boomnet_rtt", |b| {
        b.iter(|| {
            ws.send_text(true, Some(MSG.as_bytes())).unwrap();
            let mut received = 0;
            loop {
                for frame in ws.read_batch().unwrap() {
                    black_box(frame.unwrap());
                    received += 1;
                }
                if received == 100 {
                    break;
                }
            }
        })
    });

    group.finish();
}

fn boomnet_rtt_benchmark_io_service(c: &mut Criterion) {
    let mut group = c.benchmark_group("boomnet");
    group.throughput(Throughput::Bytes(MSG.len() as u64));

    // run server in the background
    server::start_on_thread(9003);

    // affinity
    core_affinity::set_for_current(core_affinity::CoreId { id: 12 });

    // setup io service
    let mut ctx = TestContext::new();
    let mut io_service = DirectSelector::new().unwrap().into_io_service_with_context(&mut ctx);
    io_service.register(TestEndpoint::new(9003, MSG));

    group.bench_function("boomnet_rtt_io_service", |b| {
        b.iter(|| {
            loop {
                io_service.poll(&mut ctx).unwrap();
                if ctx.processed == 100 {
                    ctx.wants_write = true;
                    ctx.processed = 0;
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

    // run server in the background
    server::start_on_thread(9001);

    // affinity
    core_affinity::set_for_current(core_affinity::CoreId { id: 10 });

    // setup client
    let (mut ws, _) = connect("ws://127.0.0.1:9001").unwrap();

    group.bench_function("tungstenite_rtt", |b| {
        b.iter(|| {
            ws.write(Message::Text(Utf8Bytes::from_static(MSG))).unwrap();
            ws.flush().unwrap();

            let mut received = 0;
            loop {
                if let Message::Text(data) = ws.read().unwrap() {
                    black_box(data);
                    received += 1;
                }
                if received == 100 {
                    break;
                }
            }
        })
    });

    group.finish();
}

criterion_group!(benches, boomnet_rtt_benchmark, boomnet_rtt_benchmark_io_service, tungstenite_rtt_benchmark);
criterion_main!(benches);
