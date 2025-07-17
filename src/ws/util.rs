use crate::stream::ConnectionInfo;
use crate::ws::Error;
use std::io;
use url::Url;

pub fn parse_url(url: &str) -> Result<(ConnectionInfo, String, bool), Error> {
    let url = Url::parse(url)?;
    let connection_info = ConnectionInfo::try_from(url.clone())?;
    let endpoint = match url.query() {
        Some(query) => format!("{}?{}", url.path(), query),
        None => url.path().to_string(),
    };
    let secure = match url.scheme() {
        "ws" => false,
        "wss" => true,
        scheme => Err(io::Error::other(format!("unrecognised url scheme: {scheme}")))?,
    };
    Ok((connection_info, endpoint, secure))
}
