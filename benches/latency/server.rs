use std::net::TcpListener;
use std::time::Duration;

use tungstenite::accept;

pub fn start_on_thread(port: u16) {
    let server = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    std::thread::spawn(move || {
        if let Some(stream) = server.incoming().next() {
            let mut client = accept(stream.unwrap()).unwrap();
            loop {
                let msg = client.read().unwrap();
                client.send(msg).unwrap();
            }
        }
    });
    std::thread::sleep(Duration::from_secs(1));
}
