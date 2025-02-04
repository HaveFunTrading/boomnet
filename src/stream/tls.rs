use crate::service::select::Selectable;
use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
#[cfg(feature = "openssl")]
pub use __openssl::TlsStream;
#[cfg(feature = "rustls")]
pub use __rustls::{ClientConfigExt, TlsStream};
#[cfg(feature = "mio")]
use mio::{event::Source, Interest, Registry, Token};
#[cfg(feature = "openssl")]
use openssl::ssl::SslConnectorBuilder;
#[cfg(feature = "rustls")]
use rustls::ClientConfig;
use std::fmt::Debug;
use std::io;
use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};

pub struct TlsConfig {
    #[cfg(feature = "rustls")]
    rustls_config: ClientConfig,
    #[cfg(feature = "openssl")]
    openssl_config: SslConnectorBuilder,
}

impl Deref for TlsConfig {
    #[cfg(feature = "rustls")]
    type Target = ClientConfig;
    #[cfg(feature = "openssl")]
    type Target = SslConnectorBuilder;

    fn deref(&self) -> &Self::Target {
        #[cfg(feature = "rustls")]
        {
            &self.rustls_config
        }
        #[cfg(feature = "openssl")]
        {
            &self.openssl_config
        }
    }
}

impl DerefMut for TlsConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        #[cfg(feature = "rustls")]
        {
            &mut self.rustls_config
        }
        #[cfg(feature = "openssl")]
        {
            &mut self.openssl_config
        }
    }
}

#[cfg(feature = "rustls")]
mod __rustls {
    use crate::service::select::Selectable;
    use crate::stream::tls::TlsConfig;
    use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
    use crate::util::NoBlock;
    #[cfg(feature = "mio")]
    use mio::{event::Source, Interest, Registry, Token};
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::SignatureScheme::{
        ECDSA_SHA1_Legacy, ECDSA_NISTP256_SHA256, ECDSA_NISTP384_SHA384, ECDSA_NISTP521_SHA512, ED25519, ED448,
        RSA_PKCS1_SHA1, RSA_PKCS1_SHA256, RSA_PKCS1_SHA384, RSA_PKCS1_SHA512, RSA_PSS_SHA256, RSA_PSS_SHA384,
        RSA_PSS_SHA512,
    };
    use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
    use std::fmt::Debug;
    use std::io;
    use std::io::ErrorKind::Other;
    use std::io::{Read, Write};
    use std::sync::Arc;

    pub struct TlsStream<S> {
        inner: S,
        tls: ClientConnection,
    }

    #[cfg(feature = "mio")]
    impl<S: Source> Source for TlsStream<S> {
        fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
            registry.register(&mut self.inner, token, interests)
        }

        fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
            registry.reregister(&mut self.inner, token, interests)
        }

        fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
            registry.deregister(&mut self.inner)
        }
    }

    impl<S: Selectable> Selectable for TlsStream<S> {
        fn connected(&mut self) -> io::Result<bool> {
            self.inner.connected()
        }

        fn make_writable(&mut self) {
            self.inner.make_writable()
        }

        fn make_readable(&mut self) {
            self.inner.make_readable()
        }
    }

    impl<S: Read + Write> Read for TlsStream<S> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let (_, _) = self.complete_io()?;
            self.tls.reader().read(buf)
        }
    }

    impl<S: Read + Write> Write for TlsStream<S> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.tls.writer().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.tls.writer().flush()
        }
    }

    impl<S: Read + Write> TlsStream<S> {
        pub fn wrap_with_config<F>(stream: S, server_name: &str, builder: F) -> io::Result<TlsStream<S>>
        where
            F: FnOnce(&mut TlsConfig),
        {
            #[cfg(not(all(feature = "rustls-native-certs", feature = "webpki-roots")))]
            let mut root_store = RootCertStore::empty();

            #[cfg(all(feature = "rustls-native-certs", feature = "webpki-roots"))]
            let root_store = RootCertStore::empty();

            #[cfg(all(feature = "webpki-roots", not(feature = "rustls-native-certs")))]
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

            #[cfg(all(feature = "rustls-native-certs", not(feature = "webpki-roots")))]
            {
                for cert in rustls_native_certs::load_native_certs().expect("could not load platform certs") {
                    root_store.add(cert).unwrap();
                }
            }

            let config = ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth();

            let mut config = TlsConfig { rustls_config: config };
            builder(&mut config);

            let config = Arc::new(config.rustls_config);
            let server_name = server_name.to_owned().try_into().map_err(io::Error::other)?;
            let tls = ClientConnection::new(config, server_name).map_err(io::Error::other)?;

            Ok(Self { inner: stream, tls })
        }

        pub fn wrap(stream: S, server_name: &str) -> io::Result<TlsStream<S>> {
            Self::wrap_with_config(stream, server_name, |_| {})
        }

        fn complete_io(&mut self) -> io::Result<(usize, usize)> {
            let wrote = if self.tls.wants_write() {
                self.tls.write_tls(&mut self.inner)?
            } else {
                0
            };

            let read = if self.tls.wants_read() {
                let read = self.tls.read_tls(&mut self.inner).no_block()?;
                if read > 0 {
                    self.tls
                        .process_new_packets()
                        .map_err(|err| io::Error::new(Other, err))?;
                }
                read
            } else {
                0
            };

            Ok((read, wrote))
        }
    }

    impl<S: ConnectionInfoProvider> ConnectionInfoProvider for TlsStream<S> {
        fn connection_info(&self) -> &ConnectionInfo {
            self.inner.connection_info()
        }
    }

    pub trait ClientConfigExt {
        fn with_no_cert_verification(&mut self);
    }

    impl ClientConfigExt for ClientConfig {
        fn with_no_cert_verification(&mut self) {
            self.dangerous().set_certificate_verifier(Arc::new(NoCertVerification))
        }
    }

    #[derive(Debug)]
    struct NoCertVerification;

    impl ServerCertVerifier for NoCertVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                RSA_PKCS1_SHA1,
                ECDSA_SHA1_Legacy,
                RSA_PKCS1_SHA256,
                ECDSA_NISTP256_SHA256,
                RSA_PKCS1_SHA384,
                ECDSA_NISTP384_SHA384,
                RSA_PKCS1_SHA512,
                ECDSA_NISTP521_SHA512,
                RSA_PSS_SHA256,
                RSA_PSS_SHA384,
                RSA_PSS_SHA512,
                ED25519,
                ED448,
            ]
        }
    }
}

#[cfg(feature = "openssl")]
mod __openssl {
    use crate::service::select::Selectable;
    use crate::stream::tls::TlsConfig;
    use crate::stream::{ConnectionInfo, ConnectionInfoProvider};
    #[cfg(feature = "mio")]
    use mio::{event::Source, Interest, Registry, Token};
    use openssl::ssl::{HandshakeError, MidHandshakeSslStream, SslConnector, SslMethod, SslStream};
    use std::fmt::Debug;
    use std::io;
    use std::io::ErrorKind::WouldBlock;
    use std::io::{Read, Write};

    #[derive(Debug)]
    pub struct TlsStream<S> {
        state: State<S>,
    }

    #[derive(Debug)]
    enum State<S> {
        Handshake(Option<(MidHandshakeSslStream<S>, Vec<u8>)>),
        Stream(SslStream<S>),
    }

    impl<S> State<S> {
        fn get_stream(&self) -> &S {
            match self {
                State::Handshake(stream_and_buf) => stream_and_buf.as_ref().unwrap().0.get_ref(),
                State::Stream(stream) => stream.get_ref(),
            }
        }

        fn get_stream_mut(&mut self) -> &mut S {
            match self {
                State::Handshake(stream_and_buf) => stream_and_buf.as_mut().unwrap().0.get_mut(),
                State::Stream(stream) => stream.get_mut(),
            }
        }
    }

    impl<S: ConnectionInfoProvider> ConnectionInfoProvider for State<S> {
        fn connection_info(&self) -> &ConnectionInfo {
            match self {
                State::Handshake(stream_and_buf) => stream_and_buf.as_ref().unwrap().0.get_ref().connection_info(),
                State::Stream(stream) => stream.get_ref().connection_info(),
            }
        }
    }

    #[cfg(feature = "mio")]
    impl<S: Source> Source for TlsStream<S> {
        fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
            registry.register(self.state.get_stream_mut(), token, interests)
        }

        fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
            registry.reregister(self.state.get_stream_mut(), token, interests)
        }

        fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
            registry.deregister(self.state.get_stream_mut())
        }
    }

    impl<S: Selectable> Selectable for TlsStream<S> {
        fn connected(&mut self) -> io::Result<bool> {
            self.state.get_stream_mut().connected()
        }

        fn make_writable(&mut self) {
            self.state.get_stream_mut().make_writable()
        }

        fn make_readable(&mut self) {
            self.state.get_stream_mut().make_readable()
        }
    }

    impl<S: Read + Write> Read for TlsStream<S> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match &mut self.state {
                State::Handshake(stream_and_buf) => {
                    let (mid_handshake, buffer) = stream_and_buf.take().unwrap();
                    match mid_handshake.handshake() {
                        Ok(mut ssl_stream) => {
                            // drain the pending message buffer
                            ssl_stream.write_all(&buffer)?;
                            self.state = State::Stream(ssl_stream);
                            Err(io::Error::from(WouldBlock))
                        }
                        Err(HandshakeError::WouldBlock(mid)) => {
                            self.state = State::Handshake(Some((mid, buffer)));
                            Err(io::Error::from(WouldBlock))
                        }
                        Err(_) => Err(io::Error::other("handshake failed")),
                    }
                }
                State::Stream(stream) => stream.read(buf),
            }
        }
    }

    impl<S: Read + Write> Write for TlsStream<S> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match &mut self.state {
                State::Handshake(stream_and_buf) => {
                    let (_, buffer) = stream_and_buf.as_mut().unwrap();
                    buffer.extend_from_slice(buf);
                    Ok(buf.len())
                }
                State::Stream(stream) => stream.write(buf),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            match &mut self.state {
                State::Handshake(_) => Ok(()),
                State::Stream(stream) => stream.flush(),
            }
        }
    }

    impl<S: Read + Write + Debug> TlsStream<S> {
        pub fn wrap_with_config<F>(stream: S, server_name: &str, configure: F) -> io::Result<TlsStream<S>>
        where
            F: FnOnce(&mut TlsConfig),
        {
            let mut builder = SslConnector::builder(SslMethod::tls()).map_err(io::Error::other)?;
            let mut tls_config = TlsConfig {
                openssl_config: builder,
            };
            configure(&mut tls_config);

            let connector = tls_config.openssl_config.build();
            match connector.connect(server_name, stream) {
                Ok(stream) => Ok(Self {
                    state: State::Stream(stream),
                }),
                Err(HandshakeError::WouldBlock(mid_handshake)) => Ok(Self {
                    state: State::Handshake(Some((mid_handshake, Vec::with_capacity(4096)))),
                }),
                Err(e) => Err(io::Error::other(e.to_string())),
            }
        }

        pub fn wrap(stream: S, server_name: &str) -> io::Result<TlsStream<S>> {
            Self::wrap_with_config(stream, server_name, |_| {})
        }
    }

    impl<S: ConnectionInfoProvider> ConnectionInfoProvider for TlsStream<S> {
        fn connection_info(&self) -> &ConnectionInfo {
            self.state.connection_info()
        }
    }
}

/// Trait to convert underlying stream into [TlsStream].
pub trait IntoTlsStream {
    /// Convert underlying stream into [TlsStream] with default tls config.
    ///
    /// ## Examples
    /// ```no_run
    /// use boomnet::stream::tcp::TcpStream;
    /// use boomnet::stream::tls::IntoTlsStream;
    ///
    /// let tls = TcpStream::try_from(("127.0.0.1", 4222)).unwrap().into_tls_stream();
    /// ```
    fn into_tls_stream(self) -> io::Result<TlsStream<Self>>
    where
        Self: Sized,
    {
        self.into_tls_stream_with_config(|_| {})
    }

    /// Convert underlying stream into [TlsStream] and modify tls config. The type of`TlsConfig` used
    /// will depend on whether `openssl` or `rustls` has been enabled.
    ///
    /// ## Examples
    ///
    /// Using `rustls` configure the TLS stream to disable server side certificate verification.
    /// ```no_run
    /// #[cfg(feature = "rustls")]
    /// {
    ///     use boomnet::stream::tcp::TcpStream;
    ///     use boomnet::stream::tls::{ClientConfigExt, IntoTlsStream};
    ///
    ///     let tls = TcpStream::try_from(("127.0.0.1", 4222)).unwrap().into_tls_stream_with_config(|config| {
    ///         config.with_no_cert_verification();
    ///     });
    /// }
    /// ```
    fn into_tls_stream_with_config<F>(self, builder: F) -> io::Result<TlsStream<Self>>
    where
        Self: Sized,
        F: FnOnce(&mut TlsConfig);
}

impl<T> IntoTlsStream for T
where
    T: Read + Write + Debug + ConnectionInfoProvider,
{
    fn into_tls_stream_with_config<F>(self, builder: F) -> io::Result<TlsStream<Self>>
    where
        Self: Sized,

        F: FnOnce(&mut TlsConfig),
    {
        let server_name = self.connection_info().clone().host;
        TlsStream::wrap_with_config(self, &server_name, builder)
    }
}

#[allow(clippy::large_enum_variant)]
pub enum TlsReadyStream<S> {
    Plain(S),
    Tls(TlsStream<S>),
}

impl<S: Read + Write> Read for TlsReadyStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            TlsReadyStream::Plain(stream) => stream.read(buf),
            TlsReadyStream::Tls(stream) => stream.read(buf),
        }
    }
}

impl<S: Read + Write> Write for TlsReadyStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            TlsReadyStream::Plain(stream) => stream.write(buf),
            TlsReadyStream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => stream.flush(),
            TlsReadyStream::Tls(stream) => stream.flush(),
        }
    }
}

impl<S: ConnectionInfoProvider> ConnectionInfoProvider for TlsReadyStream<S> {
    fn connection_info(&self) -> &ConnectionInfo {
        match self {
            TlsReadyStream::Plain(stream) => stream.connection_info(),
            TlsReadyStream::Tls(stream) => stream.connection_info(),
        }
    }
}

#[cfg(feature = "mio")]
impl<S: Source> Source for TlsReadyStream<S> {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => registry.register(stream, token, interests),
            TlsReadyStream::Tls(stream) => registry.register(stream, token, interests),
        }
    }

    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => registry.reregister(stream, token, interests),
            TlsReadyStream::Tls(stream) => registry.reregister(stream, token, interests),
        }
    }

    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        match self {
            TlsReadyStream::Plain(stream) => registry.deregister(stream),
            TlsReadyStream::Tls(stream) => registry.deregister(stream),
        }
    }
}

impl<S: Selectable> Selectable for TlsReadyStream<S> {
    fn connected(&mut self) -> io::Result<bool> {
        match self {
            TlsReadyStream::Plain(stream) => stream.connected(),
            TlsReadyStream::Tls(stream) => stream.connected(),
        }
    }

    fn make_writable(&mut self) {
        match self {
            TlsReadyStream::Plain(stream) => stream.make_writable(),
            TlsReadyStream::Tls(stream) => stream.make_writable(),
        }
    }

    fn make_readable(&mut self) {
        match self {
            TlsReadyStream::Plain(stream) => stream.make_readable(),
            TlsReadyStream::Tls(stream) => stream.make_readable(),
        }
    }
}
