use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

pub trait DnsResolver {
    type Query: DnsQuery;

    fn new_query(&mut self, host: impl AsRef<str>, port: u16) -> Self::Query;
}

pub trait DnsQuery {
    fn poll(&mut self) -> io::Result<impl Iterator<Item = SocketAddr>>;
}

pub struct BlockingDnsResolver;

impl DnsResolver for BlockingDnsResolver {
    type Query = BlockingDnsQuery;

    fn new_query(&mut self, host: impl AsRef<str>, port: u16) -> Self::Query {
        BlockingDnsQuery {
            host: host.as_ref().to_owned(),
            port,
            addrs: None,
        }
    }
}

pub struct BlockingDnsQuery {
    host: String,
    port: u16,
    addrs: Option<Vec<SocketAddr>>,
}

impl DnsQuery for BlockingDnsQuery {
    fn poll(&mut self) -> io::Result<impl Iterator<Item = SocketAddr>> {
        let addrs = self.addrs.get_or_insert_with(|| {
            format!("{}:{}", self.host, self.port)
                .to_socket_addrs()
                .unwrap()
                .collect()
        });
        Ok(addrs.iter().cloned())
    }
}

#[cfg(test)]
mod tests {
    use crate::service::dns::{BlockingDnsResolver, DnsQuery, DnsResolver};

    #[test]
    fn test() {
        let mut resolver = BlockingDnsResolver;
        let mut query = resolver.new_query("fstream.binance.com", 443);
        let addrs = query.poll().unwrap().collect::<Vec<_>>();
        println!("{:#?}", addrs);
    }
}
