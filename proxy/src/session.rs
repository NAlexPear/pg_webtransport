use bytes::Bytes;
use futures::AsyncWriteExt;
use http::Method;
use sec_http3::{
    ext::Protocol,
    sec_http3_quinn,
    server::Connection,
    webtransport::{
        server::{AcceptedBi, WebTransportSession},
        stream::BidiStream,
    },
};

/// Type alias for the bidirectional streams supported by the Session
pub type Stream = BidiStream<sec_http3_quinn::BidiStream<Bytes>, Bytes>;

/// Newtype wrapper around the specific flavor of WebTransport sessions that this crate uses
pub struct Session(WebTransportSession<sec_http3_quinn::Connection, Bytes>);

impl Session {
    /// Upgrade a QUIC connection to an HTTP3 connection and negotiate a new WebTransport session.
    #[tracing::instrument(skip_all, fields(remote = %connecting.remote_address()), err)]
    pub async fn start(connecting: quinn::Connecting) -> anyhow::Result<Self> {
        tracing::debug!("new connection attempted");
        let connection = connecting
            .await
            .map(sec_http3::sec_http3_quinn::Connection::new)?;

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
            "WebTransport session initiated",
        );
        Ok(Self(session))
    }

    /// Accept a bi-directional stream tied to this Session.
    #[tracing::instrument(skip(self), fields(session_id = ?self.0.session_id()), err)]
    pub async fn accept_bidirectional(&self) -> anyhow::Result<Stream> {
        tracing::debug!("Waiting for the next bi-directional stream request");

        let request = self
            .0
            .accept_bi()
            .await?
            .ok_or(anyhow::anyhow!("Unsupported stream type requested"))?;

        let AcceptedBi::BidiStream(_, mut stream) = request else {
            // FIXME: handle these additional requests over the same connection
            todo!("handle additional http3 requests over this stream");
        };

        tracing::debug!("Bidirectional Stream initiated");

        // send a greeting to the client for testing purposes
        stream.write_all(b"hello webtransport!").await?;
        Ok(stream)
    }
}
