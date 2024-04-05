// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use crate::cli::Args;
use anyhow::Error;
use futures_util::TryFutureExt;
use hyper::header::CONTENT_LENGTH;
use hyper::http::uri::Authority;
use hyper::service::{service_fn};
use hyper::{body::{Buf, Incoming as IncomingBody}, Method, Request, Response};
use hyper_util::{
  client::legacy::{Client, connect::HttpConnector},
  server::conn::auto::Builder as ServerBuilder,
  rt::{TokioExecutor, TokioIo}
};
use http_body_util::{BodyExt, Full};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::process::Child;
use std::io::Read;
use bytes::Bytes;
use tokio::net::TcpListener;

type HttpClient = Client<HttpConnector, Full<Bytes>>;

const TAURI_OPTIONS: &str = "tauri:options";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TauriOptions {
  application: PathBuf,
  #[serde(default)]
  args: Vec<String>,
  #[cfg(target_os = "windows")]
  #[serde(default)]
  webview_options: Option<Value>,
}

impl TauriOptions {
  #[cfg(target_os = "linux")]
  fn into_native_object(self) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(
      "webkitgtk:browserOptions".into(),
      json!({"binary": self.application, "args": self.args}),
    );
    map
  }

  #[cfg(target_os = "windows")]
  fn into_native_object(self) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("ms:edgeChromium".into(), json!(true));
    map.insert("browserName".into(), json!("webview2"));
    map.insert(
      "ms:edgeOptions".into(),
      json!({"binary": self.application, "args": self.args, "webviewOptions": self.webview_options}),
    );
    map
  }
}

async fn handle(
  client: HttpClient,
  req: Request<IncomingBody>,
  args: Args,
) -> Result<Response<IncomingBody>, Error> {
  let is_session_create = if let (&Method::POST, "/session") = (req.method(), req.uri().path()) {
    true
  } else {
    false
  };

  // get the body from the future stream and parse it as json
  let (mut parts, body) = req.into_parts();
  let whole_body = body.collect().await?.aggregate();

  let client_req: Request<Full<Bytes>>;

  let mut bytes = Vec::new();

  // manipulate a new session to convert options to the native driver format
  if is_session_create {
    let json: Value = serde_json::from_reader(whole_body.reader())?;

    // manipulate the json to convert from tauri option to native driver options
    let json = map_capabilities(json);

    // serialize json and update the content-length header to be accurate
    bytes = serde_json::to_vec(&json)?;
    parts.headers.insert(CONTENT_LENGTH, bytes.len().into());

    client_req = Request::from_parts(parts, Full::new(bytes.into()));
  } else {
    whole_body.reader().read_to_end(&mut bytes)?;
    client_req = Request::from_parts(parts, Full::new(bytes.into()));
  }

  client
    .request(forward_to_native_driver(client_req, args)?)
    .err_into()
    .await
}

/// Transform the request to a request for the native webdriver server.
fn forward_to_native_driver(mut req: Request<Full<Bytes>>, args: Args) -> Result<Request<Full<Bytes>>, Error> {
  let host: Authority = {
    let headers = req.headers_mut();
    headers.remove("host").expect("hyper request has host")
  }
  .to_str()?
  .parse()?;

  let path = req
    .uri()
    .path_and_query()
    .expect("hyper request has uri")
    .clone();

  let uri = format!(
    "http://{}:{}{}",
    host.host(),
    args.native_port,
    path.as_str()
  );

  let (mut parts, body) = req.into_parts();
  parts.uri = uri.parse()?;
  Ok(Request::from_parts(parts, body))
}

/// only happy path for now, no errors
fn map_capabilities(mut json: Value) -> Value {
  let mut native = None;
  if let Some(capabilities) = json.get_mut("capabilities") {
    if let Some(always_match) = capabilities.get_mut("alwaysMatch") {
      if let Some(always_match) = always_match.as_object_mut() {
        if let Some(tauri_options) = always_match.remove(TAURI_OPTIONS) {
          if let Ok(options) = serde_json::from_value::<TauriOptions>(tauri_options) {
            native = Some(options.into_native_object());
          }
        }

        if let Some(native) = native.clone() {
          always_match.extend(native);
        }
      }
    }
  }

  if let Some(native) = native {
    if let Some(desired) = json.get_mut("desiredCapabilities") {
      if let Some(desired) = desired.as_object_mut() {
        desired.remove(TAURI_OPTIONS);
        desired.extend(native);
      }
    }
  }

  json
}

#[tokio::main(flavor = "current_thread")]
pub async fn run(args: Args, mut _driver: Child) -> Result<(), Error> {
  #[cfg(unix)]
  let (signals_handle, signals_task) = {
    use futures_util::StreamExt;
    use signal_hook::consts::signal::*;

    let signals = signal_hook_tokio::Signals::new(&[SIGTERM, SIGINT, SIGQUIT])?;
    let signals_handle = signals.handle();
    let signals_task = tokio::spawn(async move {
      let mut signals = signals.fuse();
      while let Some(signal) = signals.next().await {
        match signal {
          SIGTERM | SIGINT | SIGQUIT => {
            _driver
              .kill()
              .expect("unable to kill native webdriver server");
            std::process::exit(0);
          }
          _ => unreachable!(),
        }
      }
    });
    (signals_handle, signals_task)
  };

  let address = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
  let listener = TcpListener::bind(&address).await?;

  let (stream, _) = listener.accept().await?;
  let io = TokioIo::new(stream);

  // the client we use to proxy requests to the native webdriver
  let client = Client::builder(TokioExecutor::new())
    .http1_preserve_header_case(true)
    .http1_title_case_headers(true)
    .retry_canceled_requests(false)
    .build_http();

  // pass a copy of the client to the http request handler
  let client = client.clone();
  let args = args.clone();
  let service = service_fn(move |request| {
    handle(client.clone(), request, args.clone())
  });

  // set up a http1 server that uses the service we just created
  ServerBuilder::new(TokioExecutor::new())
    .http1()
    .title_case_headers(true)
    .preserve_header_case(true)
    .serve_connection(io, service)
    .await
    .expect("server failed");

  #[cfg(unix)]
  {
    signals_handle.close();
    signals_task.await?;
  }

  Ok(())
}
