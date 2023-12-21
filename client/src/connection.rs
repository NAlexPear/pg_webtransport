use crate::log;
use bytes::BytesMut;
use fallible_iterator::FallibleIterator;
use js_sys::Uint8Array;
use postgres_protocol::{
    authentication::sasl::{ChannelBinding, ScramSha256, SCRAM_SHA_256},
    message::backend::{ErrorResponseBody, Header, Message},
};
use std::convert::TryFrom;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, WebTransportBidirectionalStream, WritableStreamDefaultWriter,
};

/// WebTransport streams and a buffer of Messages combined into a database Connection
pub struct Connection {
    read: ReadableStreamDefaultReader,
    write: WritableStreamDefaultWriter,
    pending: BytesMut,
}

impl Connection {
    /// Send Bytes of data to the writable stream
    pub async fn encode(&self, data: BytesMut) -> Result<(), JsValue> {
        let message = Uint8Array::new_with_length(data.len() as u32);
        message.copy_from(&data);
        JsFuture::from(self.write.write_with_chunk(&message)).await?;
        Ok(())
    }

    /// Read the next backend message from the stream
    // TODO: rewrite this as a Framed stream + Codec
    pub async fn decode(&mut self) -> Result<Option<Message>, JsValue> {
        loop {
            match self.decode_pending()? {
                Some(message) => return Ok(Some(message)),
                None => {
                    // if there's not at least a message's worth of data, wait for another chunk from the stream
                    let chunk = JsFuture::from(self.read.read()).await?;
                    let value = js_sys::Reflect::get(&chunk, &"value".into())
                        .map(|value| Uint8Array::new(&value))?;
                    let mut buffer = BytesMut::with_capacity(value.length() as usize);
                    unsafe {
                        // SAFETY: the Uint8Array containing this data requires equal length
                        buffer.set_len(value.length() as usize);
                    }
                    value.copy_to(&mut buffer);
                    log(&format!("chunk fetched of size {}", buffer.len()));
                    self.pending.extend_from_slice(&buffer);
                }
            }
        }
    }

    /// Decode a single message from the pending queue without re-fetching the data from upstream
    fn decode_pending(&mut self) -> Result<Option<Message>, JsValue> {
        // attempt to extract a header from the queue
        let header = Header::parse(&self.pending).map_err(|error| {
            JsValue::from(format!(
                "Error parsing the header from a backend message: {error}"
            ))
        })?;

        match header {
            // parse the Message if we have enough data to work with
            Some(header) if self.pending.len() >= (header.len() as usize + 1) => {
                return Message::parse(&mut self.pending.split_to(header.len() as usize + 1))
                    .map_err(|error| {
                        JsValue::from(format!(
                            "Error parsing the next message from the backend: {error}"
                        ))
                    });
            }

            // if there's not at least a message's worth of data, we're done
            _ => Ok(None),
        }
    }
}

/// Connection-preparing wrapper around the startup sequence of a database Connection.
/// Full Connections should only be derived by successfully completing a Startup.
pub struct Startup(Connection);

impl Startup {
    /// Run through the startup and auth sequences to prepare a Connection for real use
    pub async fn start(
        mut self,
        params: Vec<(&'static str, &'static str)>,
    ) -> Result<Connection, JsValue> {
        // send the startup message
        let mut buffer = BytesMut::new();
        postgres_protocol::message::frontend::startup_message(params, &mut buffer)
            .map_err(|error| JsValue::from(format!("Error generating startup message: {error}")))?;
        self.0.encode(buffer).await?;

        // handle the next message for authentication
        match self.0.decode().await? {
            Some(Message::AuthenticationSasl(_body)) => sasl(&mut self.0).await?,
            Some(_) => return Err(JsValue::from("Unsupported backend message type")),
            None => return Err(JsValue::from("Connection closed")),
        }

        // return the inner connection
        Ok(self.0)
    }
}

/// Generate a Startup connection from a bidirectional stream, if possible
impl TryFrom<WebTransportBidirectionalStream> for Startup {
    type Error = JsValue;

    fn try_from(stream: WebTransportBidirectionalStream) -> Result<Self, Self::Error> {
        let read = stream
            .readable()
            .get_reader()
            .dyn_into::<ReadableStreamDefaultReader>()?;

        let write = stream.writable().get_writer()?;

        Ok(Self(Connection {
            read,
            write,
            pending: BytesMut::new(),
        }))
    }
}

/// Handle SASL-based authentication
async fn sasl(connection: &mut Connection) -> Result<(), JsValue> {
    // send the initial SASL message
    let mut buffer = BytesMut::new();
    let mut scram = ScramSha256::new(b"supersecretpassword", ChannelBinding::unsupported());
    postgres_protocol::message::frontend::sasl_initial_response(
        SCRAM_SHA_256,
        scram.message(),
        &mut buffer,
    )
    .map_err(|error| {
        JsValue::from(format!(
            "Error writing SASL initial response message: {error}"
        ))
    })?;
    connection.encode(buffer).await?;

    // get the body of the SASL continuation
    let body = match connection.decode().await? {
        Some(Message::AuthenticationSaslContinue(body)) => body,
        Some(Message::ErrorResponse(body)) => return Err(format_error(body)),
        Some(_) => return Err(JsValue::from("Unexpected message during SASL handshake")),
        None => return Err(JsValue::from("Connection closed during authentication")),
    };
    scram
        .update(body.data())
        .map_err(|error| JsValue::from(format!("Error continuing SASL handshake: {error}")))?;

    // send the SASL response to the server again
    let mut buffer = BytesMut::new();
    postgres_protocol::message::frontend::sasl_response(scram.message(), &mut buffer)
        .map_err(|error| JsValue::from(format!("Error writing SASL response message: {error}")))?;
    connection.encode(buffer).await?;

    // get the body of the SASL finalizer
    let body = match connection.decode().await? {
        Some(Message::AuthenticationSaslFinal(body)) => body,
        Some(Message::ErrorResponse(body)) => return Err(format_error(body)),
        Some(_) => {
            return Err(JsValue::from(
                "Unexpected message finalizing SASL handshake",
            ))
        }
        None => return Err(JsValue::from("Connection closed during authentication")),
    };
    scram
        .finish(body.data())
        .map_err(|error| JsValue::from(format!("Error finalizing SASL handshake: {error}")))?;

    // read the connection information from the stream until we get to a terminal state
    loop {
        match connection.decode().await? {
            Some(
                Message::BackendKeyData(..)
                | Message::ParameterStatus(..)
                | Message::AuthenticationOk,
            ) => {
                // TODO: use the backend or parameter data
            }
            Some(Message::ReadyForQuery(..)) => return Ok(()),
            Some(Message::ErrorResponse(body)) => return Err(format_error(body)),
            Some(_) => return Err(JsValue::from("Unexpected backend message type")),
            None => return Err(JsValue::from("Connection closed during authentication")),
        }
    }
}

/// Format Error response bodies as useful JsValues for logging
fn format_error(body: ErrorResponseBody) -> JsValue {
    let mut fields = body.fields();
    let mut errors = "Errors: ".to_string();

    while let Ok(Some(field)) = fields.next() {
        // this is silly, but it works for now
        errors.push_str(field.value());
        errors.push_str(" ");
    }

    JsValue::from(&errors)
}
