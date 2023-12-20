use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, TextDecoder, TextEncoder, WebTransport,
    WebTransportBidirectionalStream,
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

    // initialize the "codec"
    // TODO: use postgres-protocol instead
    let encoder = TextEncoder::new()?;
    let decoder = TextDecoder::new()?;

    // read the initial dummy data
    let reader = pair
        .readable()
        .get_reader()
        .dyn_into::<ReadableStreamDefaultReader>()?;

    let chunk = JsFuture::from(reader.read()).await?;
    let value = js_sys::Reflect::get(&chunk, &"value".into())
        .and_then(|value| decoder.decode_with_buffer_source(&value.into()))?;

    log(&value);

    // write something back
    let writer = pair.writable().get_writer()?;
    let message = encoder.encode_with_input("test string again");
    let buffer = js_sys::Uint8Array::new_with_length(message.len() as u32);
    buffer.copy_from(&message);
    JsFuture::from(writer.write_with_chunk(&buffer)).await?;

    Ok(())
}
