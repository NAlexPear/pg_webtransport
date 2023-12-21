use bytes::BytesMut;
use connection::Startup;
use js_sys::Uint8Array;
use postgres_protocol::message::backend::Message;
use std::convert::TryFrom;
use wasm_bindgen::{prelude::wasm_bindgen, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{WebTransport, WebTransportBidirectionalStream};

mod connection;
mod utils;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[wasm_bindgen]
pub async fn run() -> Result<(), JsValue> {
    // TODO: turn this into a real interface on the JS side
    utils::set_panic_hook();

    // initialize the WebTransport channel
    let transport = WebTransport::new("https://127.0.0.1:4433")?;
    JsFuture::from(transport.ready()).await?;
    log("WebTransport ready!");

    // start a bidirectional stream
    let pair: WebTransportBidirectionalStream =
        JsFuture::from(transport.create_bidirectional_stream())
            .await?
            .into();

    // run through the startup process to get a real Connection
    let startup_params = vec![
        ("client_encoding", "UTF8"),
        ("user", "postgres"),
        ("database", "postgres"),
        ("application_name", "webtransport"),
    ];
    let mut connection = Startup::try_from(pair)?.start(startup_params).await?;

    log("Connection ready.");

    // TODO: invoke some separate parse + bind + execute sequences for testing
    // TODO: extract this PARSE + DESCRIBE + SYNC + BIND + EXECUTE flow into a convenience function

    // create a parse message for a simple query against the unnamed portal
    let mut buffer = BytesMut::new();
    postgres_protocol::message::frontend::parse(
        "",
        "select current_user, now(), 'hello world' as greeting",
        [],
        &mut buffer,
    )
    .map_err(|error| JsValue::from(&format!("Failed to generate Parse message: ${error}")))?;
    connection.encode(buffer).await?;

    // TODO: issue a Describe and Sync to get type information for callers, too

    // bind the parameters (none in this case) to the unnamed prepared statement
    let mut buffer = BytesMut::new();
    postgres_protocol::message::frontend::bind(
        "",
        "",
        [],
        [],
        |_: i32, _| Ok(postgres_protocol::IsNull::No), // FIXME: use a real serializer here
        Some(1),
        &mut buffer,
    )
    .map_err(|_| JsValue::from("Failed to generate Bind message"))?;
    connection.encode(buffer).await?;

    // execute the query
    let mut buffer = BytesMut::new();
    postgres_protocol::message::frontend::execute("", 0, &mut buffer)
        .map_err(|_| JsValue::from("Failed to generate Execute message"))?;
    connection.encode(buffer).await?;

    // issue a Sync to get all of the messages we need from the backend
    let mut buffer = BytesMut::new();
    postgres_protocol::message::frontend::sync(&mut buffer);
    connection.encode(buffer).await?;

    // make sure we get stuff back
    while let Some(message) = connection.decode().await? {
        match message {
            Message::DataRow(body) => {
                // TODO: send these bytes back raw (with type info) to JS-land for parsing
                let data = body.buffer_bytes();
                let message = Uint8Array::new_with_length(data.len() as u32);
                message.copy_from(&data);
                let output = web_sys::TextDecoder::new()?.decode_with_buffer_source(&message)?;
                log(&format!("Data returned: {output}"));
            }
            Message::ReadyForQuery(..) => {
                log("Ready for the next query!");
                break;
            }
            Message::ParseComplete
            | Message::BindComplete
            | Message::CommandComplete(..)
            | Message::EmptyQueryResponse
            | Message::PortalSuspended => {
                // these are expected, so the loop can continue
            }
            Message::ErrorResponse(..) => {
                return Err(JsValue::from("Error response returned from the query"))
            }
            _ => return Err(JsValue::from("Unexpected message returned from the query")),
        }
    }

    // TODO: use the connection as a Stream + Sink
    // TODO: give callers from JS-land a useful Client for querying

    Ok(())
}
