use connection::Startup;
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

    // run through the startup process to get a real Connection
    let startup_params = vec![
        ("client_encoding", "UTF8"),
        ("user", "postgres"),
        ("database", "postgres"),
        ("application_name", "webtransport"),
    ];
    let _connection = Startup::try_from(pair)?
        .start(startup_params)
        .await?
        .authenticate()
        .await?;
    log("Connection ready.");

    // TODO: use the connection as a Stream + Sink
    // TODO: invoke some separate parse + bind + execute sequences for testing

    Ok(())
}
