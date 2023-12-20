use crate::session::Stream;
use anyhow::Context;
use std::net::SocketAddrV4;
use tokio::net::TcpStream;

/// Bi-directional proxy between a WebTransport Stream and a TCP connection
pub struct Proxy;

impl Proxy {
    /// Start consuming a Stream, copying both the read and write half of the stream to a TCP
    /// connection until either side disconnects or emits an error.
    #[tracing::instrument(skip(stream), err)]
    pub async fn start(mut stream: Stream, upstream_port: u16) -> anyhow::Result<()> {
        tracing::debug!("Starting proxy connection");

        // derive an address for the upstream socket
        let upstream = SocketAddrV4::new([127, 0, 0, 1].into(), upstream_port);

        // connect to the upstream socket using TCP
        let mut tcp = TcpStream::connect(upstream)
            .await
            .context("Failed to connect to upstream TCP target")?;

        // copy between the stream and the socket in both directions
        tokio::io::copy_bidirectional(&mut stream, &mut tcp)
            .await
            .context("Proxy connection disconnected")?;

        tracing::debug!("Proxy connection closing");

        Ok(())
    }
}
