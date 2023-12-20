use futures::Stream;
use rustls::ServerConfig;
use std::{net::SocketAddrV4, sync::Arc, time::Duration};

/// QUIC connection-listener server
pub struct Endpoint {
    server_config: quinn::ServerConfig,
}

impl Endpoint {
    /// Create a new Endpoint from a TLS configuration
    pub fn new(tls: ServerConfig) -> Self {
        let mut transport_config = quinn::TransportConfig::default();
        transport_config.keep_alive_interval(Some(Duration::from_secs(2)));
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(tls));
        server_config.transport_config(transport_config.into());
        Self { server_config }
    }

    /// Listen on a specific port using this Endpoint's configuration
    #[tracing::instrument(skip(self), err)]
    pub fn listen(self, port: u16) -> anyhow::Result<impl Stream<Item = quinn::Connecting>> {
        let address = SocketAddrV4::new([127, 0, 0, 1].into(), port);
        let endpoint = quinn::Endpoint::server(self.server_config, address.into())?;
        let connection_attempts = futures::stream::unfold(endpoint, |endpoint| async {
            endpoint.accept().await.map(|attempt| (attempt, endpoint))
        });
        tracing::info!("listening for new connections");
        Ok(connection_attempts)
    }
}
