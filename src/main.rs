use anyhow::Context;
use bytes::Bytes;
use clap::Parser;
use futures::AsyncWriteExt;
use http::Method;
use rustls::{Certificate, PrivateKey};
use sec_http3::{
    ext::Protocol,
    server::Connection,
    webtransport::server::{AcceptedBi, WebTransportSession},
};
use std::{net::SocketAddrV4, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tracing_subscriber::EnvFilter;

// TODO: what's the *actual* goal with this project?
//
// three levels of proxying
// 1. just a WebTransport <-> TCP proxy (postgres is just one thing that speaks TCP, so we get
//    that for "free", but it's up to the client to handle the entire connection/protocol)
// 2. a WebTransport <-> (postgres startup sequence on TCP connection) + TCP proxy
//    (negotiating the actual upstream handshake then forwarding subsequent bytes)
// 3. a higher-level query interface (statement + parameters)
//
// we should start with the first, then figure out how to integrate this interface
// with existing web auth strategies to handle the second case
// (e.g. using NGINX + Kratos to auth via Cookie and set the Postgres user via header)

/// Startup values for this server, provided by arguments when the binary is invoked.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Configuration {
    /// path to a DER-encoded cert file
    #[arg(short, long, default_value = "./certs/localhost.crt")]
    cert: PathBuf,

    /// path to a DER-encoded key file
    #[arg(short, long, default_value = "./certs/localhost.key")]
    key: PathBuf,

    /// port that the server will listen on
    #[arg(short, long, default_value = "4433")]
    port: u16,

    /// port of the TCP service that is being proxied
    #[arg(short, long, default_value = "5432")]
    upstream_port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // configure logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // generate configuration values from arguments
    let configuration = Configuration::parse();
    let cert = Certificate(std::fs::read(configuration.cert)?);
    let key = PrivateKey(std::fs::read(configuration.key)?);

    // set up the TLS configuration for the server
    let mut tls_config = rustls::ServerConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;

    // handle ALPN protocols
    tls_config.max_early_data_size = u32::MAX;
    let alpn: Vec<Vec<u8>> = vec![
        b"h3".to_vec(),
        b"h3-32".to_vec(),
        b"h3-31".to_vec(),
        b"h3-30".to_vec(),
        b"h3-29".to_vec(),
    ];
    tls_config.alpn_protocols = alpn;

    // set up the QUIC endpoint listener corresponding to a single UDP socket that may host many connections
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.keep_alive_interval(Some(Duration::from_secs(2)));
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(tls_config));
    server_config.transport_config(transport_config.into());
    let address = SocketAddrV4::new([127, 0, 0, 1].into(), configuration.port);
    let endpoint = quinn::Endpoint::server(server_config, address.into())?;
    tracing::info!("listening at {address}");

    // spawn tasks to handle each QUIC connection attempt
    while let Some(connection_attempt) = endpoint.accept().await {
        tokio::spawn(async move {
            if let Err(error) = async {
                // generate the Session and an initial bidirectional stream from the connection attempt
                let session = start_session(connection_attempt).await?;

                // handle bidirectional streams, since that's the only type of WebTransport
                // type that makes sense for a database connection
                let request = session
                    .accept_bi()
                    .await?
                    .ok_or(anyhow::anyhow!("Unsupported stream type requested"))?;

                let AcceptedBi::BidiStream(_, mut stream) = request else {
                    // FIXME: handle these additional requests over the same connection
                    todo!("handle additional http3 requests over this stream");
                };

                tracing::debug!(
                    session_id = ?session.session_id(),
                    "Bidirectional Stream initiated"
                );

                // send a greeting to the client for testing purposes
                stream.write_all(b"hello webtransport!").await?;

                // proxy this WebTransport stream to a TcpStream
                let upstream =
                    SocketAddrV4::new([127, 0, 0, 1].into(), configuration.upstream_port);
                let mut tcp = TcpStream::connect(upstream)
                    .await
                    .context("Failed to connect to upstream TCP target")?;
                tokio::io::copy_bidirectional(&mut stream, &mut tcp).await?;
                tracing::debug!("finished with bidirectional stream");
                anyhow::Ok(())
            }
            .await
            {
                tracing::error!(%error, "Stream error");
            }
        });
    }

    Ok(())
}

/// Type alias for the flavor of WebTransport session that this server uses
type Session = WebTransportSession<sec_http3::sec_http3_quinn::Connection, Bytes>;

/// Upgrade a QUIC connection to an HTTP3 connection and negotiate a new WebTransport session.
#[tracing::instrument(skip(connection_attempt))]
async fn start_session(connection_attempt: quinn::Connecting) -> anyhow::Result<Session> {
    tracing::debug!(
        remote = %connection_attempt.remote_address(),
        "new connection attempted",
    );
    let connection = connection_attempt.await?;
    let connection = sec_http3::sec_http3_quinn::Connection::new(connection);
    let mut h3: Connection<_, Bytes> = sec_http3::server::builder()
        .enable_webtransport(true)
        .enable_connect(true)
        .enable_datagram(true)
        .max_webtransport_sessions(1)
        .send_grease(true)
        .build(connection)
        .await?;

    tracing::debug!("new HTTP/3 connection established");

    // handle stream requests over the new HTTP3 connection
    // TODO: we should be able to establish multiple WebTransport Sessions
    // over the same underlying HTTP/3 connection, right? That'd require something custom
    // for the third argument to WebTransportSession::accept, though, as that takes
    // ownership of the *entire* h3 connection.
    let (request, stream) = h3
        .accept()
        .await?
        .ok_or(anyhow::anyhow!("Connection closed"))?;

    // verify that this is really a WebTransport request
    let extensions = request.extensions();
    anyhow::ensure!(
        matches!(request.method(), &Method::CONNECT),
        "Request was not a proper CONNECT",
    );
    anyhow::ensure!(
        extensions.get() == Some(&Protocol::WEB_TRANSPORT),
        "Request was not using the WEB_TRANSPORT protocol",
    );
    tracing::debug!("new WebTransport session requested");

    // build a real session from this request
    let session = WebTransportSession::accept(request, stream, h3).await?;
    tracing::debug!(
        session_id = ?session.session_id(),
        "WebTransport session initiated"
    );
    Ok(session)
}
