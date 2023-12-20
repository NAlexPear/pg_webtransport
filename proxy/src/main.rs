use clap::Parser;
use endpoint::Endpoint;
use futures::{StreamExt, TryFutureExt};
use proxy::Proxy;
use rustls::{Certificate, PrivateKey};
use session::Session;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod endpoint;
mod proxy;
mod session;

// TODO: switch over to wtransport for a simpler server, perhaps?
// https://github.com/BiagioFesta/wtransport

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
    Endpoint::new(tls_config)
        .listen(configuration.port)?
        .for_each(|connection_attempt| async {
            // spawn a task to handle each QUIC connection attempt
            tokio::spawn(
                async move {
                    let session = Session::start(connection_attempt).await?;
                    let stream = session.accept_bidirectional().await?;
                    Proxy::start(stream, configuration.upstream_port).await
                }
                .inspect_err(|error| {
                    tracing::error!(%error, "Stream error");
                }),
            );
        })
        .await;

    Ok(())
}
