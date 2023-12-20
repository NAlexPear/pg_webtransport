use bytes::BytesMut;
use fallible_iterator::FallibleIterator;
use js_sys::Uint8Array;
use postgres_protocol::{
    authentication::sasl::{ChannelBinding, ScramSha256, SCRAM_SHA_256},
    message::backend::Message,
};
use std::collections::HashMap;
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, WebTransport, WebTransportBidirectionalStream,
    WritableStreamDefaultWriter,
};

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
    log("Hello from WASM!");

    // initialize the WebTransport channel
    let transport = WebTransport::new("https://127.0.0.1:4433")?;
    JsFuture::from(transport.ready()).await?;
    log("WebTransport ready for use!");

    // start a bidirectional stream
    let pair: WebTransportBidirectionalStream =
        JsFuture::from(transport.create_bidirectional_stream())
            .await?
            .into();

    // create a startup message
    let startup_params = vec![
        ("client_encoding", "UTF8"),
        ("user", "postgres"),
        ("database", "postgres"),
        ("application_name", "webtransport"),
    ];
    let mut buffer = BytesMut::new();
    postgres_protocol::message::frontend::startup_message(startup_params, &mut buffer)
        .map_err(|error| JsValue::from(format!("Error writing startup message: {error}")))?;
    let message = Uint8Array::new_with_length(buffer.len() as u32);
    message.copy_from(&buffer);

    // write the startup message to the stream first
    let writer = pair.writable().get_writer()?;
    JsFuture::from(writer.write_with_chunk(&message)).await?;

    // make sure that we can read the response
    let reader = pair
        .readable()
        .get_reader()
        .dyn_into::<ReadableStreamDefaultReader>()?;

    // authenticate the connection, returning process and session info
    let (process_id, _secret_key, _parameters) = authenticate(&reader, &writer).await?;
    log(&format!("process {process_id} ready for queries!"));

    Ok(())
}

/// Handle the authentication handshake, specifically
// TODO: this needs a bunch of help/cleanup/extension
async fn authenticate(
    reader: &ReadableStreamDefaultReader,
    writer: &WritableStreamDefaultWriter,
) -> Result<(i32, i32, HashMap<String, String>), JsValue> {
    // read the next message from the database
    // TODO: this should probably be a custom stream + Codec
    let chunk = JsFuture::from(reader.read()).await?;
    let value =
        js_sys::Reflect::get(&chunk, &"value".into()).map(|value| Uint8Array::new(&value))?;
    let mut buffer = BytesMut::with_capacity(value.length() as usize);
    unsafe {
        // SAFETY: the Uint8Array containing this data requires equal length
        buffer.set_len(value.length() as usize);
    }
    value.copy_to(&mut buffer);
    let message = Message::parse(&mut buffer).map_err(|error| {
        JsValue::from(format!(
            "Error parsing the next message from the backend: {error}"
        ))
    })?;

    match message {
        Some(Message::AuthenticationSasl(_body)) => {
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
            let message = Uint8Array::new_with_length(buffer.len() as u32);
            message.copy_from(&buffer);
            JsFuture::from(writer.write_with_chunk(&message)).await?;

            // get the body of the SASL continuation
            let chunk = JsFuture::from(reader.read()).await?;
            let value = js_sys::Reflect::get(&chunk, &"value".into())
                .map(|value| Uint8Array::new(&value))?;
            let mut buffer = BytesMut::with_capacity(value.length() as usize);
            unsafe {
                // SAFETY: the Uint8Array containing this data requires equal length
                buffer.set_len(value.length() as usize);
            }
            value.copy_to(&mut buffer);
            let message = Message::parse(&mut buffer).map_err(|error| {
                JsValue::from(format!(
                    "Error parsing the next message from the backend: {error}"
                ))
            })?;
            let body = match message {
                Some(Message::AuthenticationSaslContinue(body)) => body,
                Some(Message::ErrorResponse(body)) => {
                    let mut fields = body.fields();
                    let mut errors = "Errors: ".to_string();

                    while let Ok(Some(field)) = fields.next() {
                        log(field.value());
                        errors.push_str(field.value()); // FIXME: this is silly
                        errors.push_str(" "); // FIXME: this is silly
                    }

                    return Err(JsValue::from(&errors));
                }
                Some(_) => return Err(JsValue::from("Unexpected message during SASL handshake")),
                None => return Err(JsValue::from("Connection closed")),
            };
            scram.update(body.data()).map_err(|error| {
                JsValue::from(format!("Error continuing SASL handshake: {error}"))
            })?;

            // send the SASL response to the server again
            let mut buffer = BytesMut::new();
            postgres_protocol::message::frontend::sasl_response(scram.message(), &mut buffer)
                .map_err(|error| {
                    JsValue::from(format!("Error writing SASL response message: {error}"))
                })?;
            let message = Uint8Array::new_with_length(buffer.len() as u32);
            message.copy_from(&buffer);
            JsFuture::from(writer.write_with_chunk(&message)).await?;

            // get the body of the SASL finalizer
            let chunk = JsFuture::from(reader.read()).await?;
            let value = js_sys::Reflect::get(&chunk, &"value".into())
                .map(|value| Uint8Array::new(&value))?;
            let mut buffer = BytesMut::with_capacity(value.length() as usize);
            unsafe {
                // SAFETY: the Uint8Array containing this data requires equal length
                buffer.set_len(value.length() as usize);
            }
            value.copy_to(&mut buffer);
            let message = Message::parse(&mut buffer).map_err(|error| {
                JsValue::from(format!(
                    "Error parsing the next message from the backend: {error}"
                ))
            })?;
            let body = match message {
                Some(Message::AuthenticationSaslFinal(body)) => body,
                Some(Message::ErrorResponse(body)) => {
                    let mut fields = body.fields();
                    let mut errors = "Errors: ".to_string();

                    while let Ok(Some(field)) = fields.next() {
                        log(field.value());
                        errors.push_str(field.value()); // FIXME: this is silly
                        errors.push_str(" "); // FIXME: this is silly
                    }

                    return Err(JsValue::from(&errors));
                }
                Some(_) => {
                    return Err(JsValue::from(
                        "Unexpected message finalizing SASL handshake",
                    ))
                }
                None => return Err(JsValue::from("Connection closed")),
            };
            scram.finish(body.data()).map_err(|error| {
                JsValue::from(format!("Error finalizing SASL handshake: {error}"))
            })?;

            read_session_info(&mut buffer).await
        }
        Some(_) => Err(JsValue::from("Unsupported backend message type")),
        None => Err(JsValue::from("Connection closed")),
    }
}

async fn read_session_info(
    buffer: &mut BytesMut,
) -> Result<(i32, i32, HashMap<String, String>), JsValue> {
    // read the next message from the previous query
    // TODO: this should absolutely be a custom stream + Codec
    let message = Message::parse(buffer).map_err(|error| {
        JsValue::from(format!(
            "Error parsing the next message from the backend: {error}"
        ))
    })?;

    let mut process_id = 0;
    let mut secret_key = 0;
    let mut parameters = HashMap::new();

    match message {
        Some(Message::BackendKeyData(body)) => {
            process_id = body.process_id();
            secret_key = body.secret_key();
        }
        Some(Message::ParameterStatus(body)) => {
            let name = body
                .name()
                .map_err(|_| JsValue::from("Invalid parameter key"))?
                .to_string();

            let value = body
                .value()
                .map_err(|_| JsValue::from("Invalid parameter value"))?
                .to_string();

            parameters.insert(name, value);
        }
        Some(Message::ReadyForQuery(_) | Message::AuthenticationOk) => {}
        Some(Message::ErrorResponse(body)) => {
            let mut fields = body.fields();
            let mut errors = "Errors: ".to_string();

            while let Ok(Some(field)) = fields.next() {
                log(field.value());
                errors.push_str(field.value()); // FIXME: this is silly
                errors.push_str(" "); // FIXME: this is silly
            }

            return Err(JsValue::from(&errors));
        }
        Some(_) => return Err(JsValue::from("Unexpected backend message type")),
        None => return Err(JsValue::from("Connection closed")),
    }

    Ok((process_id, secret_key, parameters))
}
