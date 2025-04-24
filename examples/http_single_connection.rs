use boomnet::http::{HttpClient, SingleTlsConnectionPool};
use http::Method;

fn main() -> anyhow::Result<()> {
    let pool = SingleTlsConnectionPool::new(("fapi.binance.com", 443).into());
    let mut client = HttpClient::new(pool);
    let mut request = client.new_request(Method::GET, "/fapi/v1/time")?;

    loop {
        if let Some((status_code, headers, body)) = request.poll()? {
            println!("{}", status_code);
            println!("{}", headers);
            println!("{}", body);
            break;
        }
    }

    Ok(())
}
