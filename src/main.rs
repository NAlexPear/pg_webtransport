use bytes::Bytes;
use futures::{AsyncReadExt, AsyncWriteExt};
use http::Method;
use rustls::{Certificate, PrivateKey};
use sec_http3::{
    ext::Protocol,
    quic,
    server::Connection,
    webtransport::server::{AcceptedBi::BidiStream, WebTransportSession},
};
use std::{net::SocketAddrV4, sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // FIXME: take configuration from the environment/envy
    let cert = Certificate(std::fs::read("./certs/localhost.crt")?);
    let key = PrivateKey(std::fs::read("./certs/localhost.key")?);

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

    // set up the QUIC endpoint listener
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.keep_alive_interval(Some(Duration::from_secs(2)));
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(tls_config));
    server_config.transport_config(transport_config.into());
    let address = SocketAddrV4::new([127, 0, 0, 1].into(), 4433);
    let endpoint = quinn::Endpoint::server(server_config, address.into())?;
    println!("listening at {address}");

    // spawn tasks to handle each QUIC connection
    // FIXME: see if we can't use streams to make this runtime-agnostic
    // FIXME: spawn this work as tasks (or push to a localset or something)
    while let Some(connection_attempt) = endpoint.accept().await {
        println!("new connection attempted!");
        let connection = connection_attempt.await?;
        println!("new connection established!");
        let mut h3: Connection<_, Bytes> = sec_http3::server::builder()
            .enable_webtransport(true)
            .enable_connect(true)
            .enable_datagram(true)
            .max_webtransport_sessions(1)
            .send_grease(true)
            .build(sec_http3::sec_http3_quinn::Connection::new(connection))
            .await?;
        println!("h3 connection established!");

        // handle stream requests over the new HTTP3 connection
        // FIXME: we'll need more than one!
        if let Some((request, stream)) = h3.accept().await? {
            let extensions = request.extensions();
            if matches!(request.method(), &Method::CONNECT,)
                && extensions.get() == Some(&Protocol::WEB_TRANSPORT)
            {
                println!("it's a webtransport session!");
                let session = WebTransportSession::accept(request, stream, h3).await?;

                // handle bidirectional streams, since that's the only type of WebTransport
                // type that makes sense for a database connection
                if let Some(BidiStream(_session_id, stream)) = session.accept_bi().await? {
                    println!("bidirectional stream initiated");
                    let (mut tx, mut rx) = quic::BidiStream::split(stream);
                    tx.write_all(b"hello webtransport!").await?;
                    let mut buffer = Vec::new();
                    rx.read_to_end(&mut buffer).await?;
                    println!("finished with bidi stream");
                }
            }
        }

        println!("connection no longer accepting session requests");
    }

    Ok(())
}
