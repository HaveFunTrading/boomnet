use boomnet::http::{HttpClient, SingleTlsConnectionPool};
use http::Method;

fn main() -> anyhow::Result<()> {
    let pool = SingleTlsConnectionPool::new(("fapi.binance.com", 443));
    let mut client = HttpClient::new(pool);

    let request = client.new_request_with_headers(Method::GET, "/fapi/v1/time", |headers| {
        headers[0] = ("FOO", "bar");
        1
    })?;

    // execute in blocking mode (will consume request)
    let (status_code, headers, body) = request.block()?;
    println!("{}", status_code);
    println!("{}", headers);
    println!("{}", body);

    // execute in async mode
    let mut request = client.new_request(Method::GET, "/fapi/v1/time")?;
    loop {
        if let Some((status_code, headers, body)) = request.poll()? {
            println!("{}", status_code);
            println!("{}", headers);
            println!("{}", body);
            break;
        }
    }
    
    // once the request is done polling it again will just return the same data
    let (status_code, headers, body) = request.poll()?.unwrap();
    println!("{}", status_code);
    println!("{}", headers);
    println!("{}", body);

    Ok(())
}
