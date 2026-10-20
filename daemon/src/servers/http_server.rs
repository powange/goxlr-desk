use actix_cors::Cors;
use actix_multipart::Multipart;
use actix_web::body::MessageBody;
use actix_web::dev::{ServerHandle, ServiceRequest, ServiceResponse};
use actix_web::error::ErrorForbidden;
use actix_web::http::header;
use actix_web::http::header::ContentType;
use actix_web::middleware::{Condition, Next, from_fn};
use actix_web::web::Data;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, get, post, web};
use actix_ws::{AggregatedMessage, CloseCode, CloseReason, Session};
use anyhow::{Result, anyhow};
use enum_map::EnumMap;
use futures_util::StreamExt;
use include_dir::{Dir, include_dir};
use jsonpath_rust::JsonPath;
use log::{debug, error, info, warn};
use mime_guess::MimeGuess;
use mime_guess::mime::IMAGE_PNG;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Component, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};
use sysinfo::System;
use tokio::sync::broadcast::Sender as BroadcastSender;
use tokio::sync::oneshot::Sender;
use tokio::sync::{RwLock, oneshot};

use crate::PatchEvent;
use crate::files::{FilePaths, find_file_in_path};
use goxlr_ipc::{
    DaemonCommand, DaemonRequest, DaemonResponse, DaemonStatus, HttpSettings, Scribble,
    WebsocketRequest, WebsocketResponse,
};
use goxlr_scribbles::get_scribble_png;
use goxlr_types::FaderName;

use crate::primary_worker::{DeviceCommand, DeviceSender};
use crate::servers::server_packet::handle_packet;

const WEB_CONTENT: Dir = include_dir!("./daemon/web-content/");

// Firmware files are a few megabytes, anything far beyond this isn't a firmware.
const MAX_FIRMWARE_SIZE: usize = 64 * 1024 * 1024;

/// Decides which requests may reach the HTTP server. Browsers let any web page send requests to
/// localhost (WebSockets and form posts aren't blocked by CORS), so we check the Host and Origin
/// headers to make sure a request comes from our own web UI and not from another site, or from a
/// DNS rebinding domain pointing at this machine.
struct RequestPolicy {
    cors_enabled: bool,
    host_names: Vec<String>,
}

impl RequestPolicy {
    fn new(cors_enabled: bool, bind_address: &str) -> Self {
        let mut host_names = vec![String::from("localhost"), bind_address.to_ascii_lowercase()];
        if let Some(name) = System::host_name() {
            let name = name.to_ascii_lowercase();
            host_names.push(format!("{name}.local"));
            host_names.push(name);
        }
        Self {
            cors_enabled,
            host_names,
        }
    }

    /// A Host is accepted if it's an IP address, or a name that belongs to this machine.
    fn is_host_allowed(&self, host: &str) -> bool {
        let name = strip_port(host).to_ascii_lowercase();
        name.parse::<IpAddr>().is_ok()
            || name.ends_with(".localhost")
            || self.host_names.contains(&name)
    }

    /// An Origin is accepted if it matches the Host (the request comes from a page we served), or
    /// if CORS is enabled and the page is served from this machine's loopback address.
    fn is_origin_allowed(&self, origin: &str, host: Option<&str>) -> bool {
        let Some(authority) = strip_scheme(origin) else {
            return false;
        };
        if let Some(host) = host
            && authority.eq_ignore_ascii_case(host)
        {
            return true;
        }
        self.cors_enabled && is_loopback_name(strip_port(authority))
    }
}

/// Returns the 'host[:port]' part of an http(s) origin
fn strip_scheme(origin: &str) -> Option<&str> {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
}

/// Removes the port from a 'host[:port]' string, handling bracketed IPv6 addresses
fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(name, _)| name)
}

fn is_loopback_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn is_loopback_origin(origin: &str) -> bool {
    strip_scheme(origin).is_some_and(|authority| is_loopback_name(strip_port(authority)))
}

/// Whether the request comes from this machine (as opposed to another machine on the network,
/// which is only possible when network access is enabled).
fn is_local_peer(req: &HttpRequest) -> bool {
    req.peer_addr()
        .is_some_and(|addr| addr.ip().to_canonical().is_loopback())
}

/// Requests that run a program or flash a file from this machine's disk are refused when they
/// come from another machine, the HTTP API has no authentication.
fn local_only_reason(request: &DaemonRequest) -> Option<&'static str> {
    match request {
        DaemonRequest::Daemon(DaemonCommand::SetActivatorPath(_)) => {
            Some("The activator can only be changed from this computer")
        }
        DaemonRequest::RunFirmwareUpdate(_, Some(_), _) => {
            Some("A firmware file on disk can only be flashed from this computer")
        }
        _ => None,
    }
}

async fn check_request_source(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let rejection = {
        let headers = req.headers();
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
        let origin = headers
            .get(header::ORIGIN)
            .map(|o| o.to_str().unwrap_or_default());

        // Requests without these headers don't come from a browser, so they can't be forged
        // by a web page, and are let through.
        let allowed = match req.app_data::<Data<RequestPolicy>>() {
            Some(policy) => {
                host.is_none_or(|host| policy.is_host_allowed(host))
                    && origin.is_none_or(|origin| policy.is_origin_allowed(origin, host))
            }
            None => false,
        };

        (!allowed).then(|| format!("Host: {host:?}, Origin: {origin:?}"))
    };

    if let Some(details) = rejection {
        warn!("Rejected HTTP request to {} ({})", req.path(), details);
        return Err(ErrorForbidden("Request rejected, unknown Host or Origin"));
    }
    next.call(req).await
}

struct AppData {
    usb_tx: DeviceSender,
    broadcast_tx: BroadcastSender<PatchEvent>,
    file_paths: FilePaths,

    scribble_state: EnumMap<FaderName, ScribbleState>,
}

#[derive(Debug, Default)]
struct ScribbleState {
    scribble_config: Option<Scribble>,
    png_data: Option<Vec<u8>>,
}

pub async fn spawn_http_server(
    usb_tx: DeviceSender,
    handle_tx: Sender<Result<Option<ServerHandle>>>,
    broadcast_tx: tokio::sync::broadcast::Sender<PatchEvent>,
    settings: HttpSettings,
    file_paths: FilePaths,
) {
    // Create the AppData ONCE, outside the closure
    let app_data = Data::new(RwLock::new(AppData {
        broadcast_tx: broadcast_tx.clone(),
        usb_tx: usb_tx.clone(),
        file_paths: file_paths.clone(),
        scribble_state: EnumMap::default(),
    }));
    let policy = Data::new(RequestPolicy::new(
        settings.cors_enabled,
        &settings.bind_address,
    ));

    let server = HttpServer::new(move || {
        let cors = Cors::default()
            .allowed_origin_fn(|origin, _req_head| origin.to_str().is_ok_and(is_loopback_origin))
            .allow_any_method()
            .allow_any_header()
            .max_age(300);
        App::new()
            .wrap(Condition::new(settings.cors_enabled, cors))
            .wrap(from_fn(check_request_source))
            .app_data(policy.clone())
            .app_data(app_data.clone())
            .service(websocket)
            .service(execute_command)
            .service(get_devices)
            .service(get_sample)
            .service(get_scribble)
            .service(get_path)
            .service(upload_firmware)
            .default_service(web::to(default))
    })
    .bind((settings.bind_address.clone(), settings.port));

    if let Err(e) = server {
        // Log the Error Message..
        warn!("Unable to Start HTTP Server: {}", e);

        // Let 'Upstream' know an error has occurred
        let _ = handle_tx.send(Err(anyhow!(e)));

        // Give up :D
        return;
    }

    // Run the server..
    let server = server.unwrap().run();
    info!(
        "Started GoXLR configuration interface at http://{}:{}/",
        settings.bind_address.as_str(),
        settings.port,
    );
    let loopback = settings
        .bind_address
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
        || settings.bind_address.eq_ignore_ascii_case("localhost");
    if !loopback {
        warn!(
            "The configuration interface is reachable from other machines and has no password, \
            only enable network access on networks you trust."
        );
    }

    // Let upstream know we're running...
    let _ = handle_tx.send(Ok(Some(server.handle())));

    // Wait for the server to exit with its reason
    let result = server.await;
    if result.is_err() {
        error!("HTTP Server Stopped with Error: {}", result.err().unwrap());
        return;
    }

    info!("HTTP Server Stopped.");
}

#[derive(Serialize)]
struct WsResponse(WebsocketResponse);

#[get("/api/websocket")]
async fn websocket(
    app_data: Data<RwLock<AppData>>,
    req: HttpRequest,
    body: web::Payload,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, mut session, msg_stream) = actix_ws::handle(&req, body)?;
    let local_peer = is_local_peer(&req);

    let data = app_data.read().await;
    let mut usb_tx = data.usb_tx.clone();
    let mut broadcast_rx = data.broadcast_tx.subscribe();

    // Spawn the handler (this is now where we do stuff)
    actix_web::rt::spawn(async move {
        let mut msg_stream = msg_stream.aggregate_continuations();

        let close_reason = loop {
            tokio::select! {
                Ok(patch) = broadcast_rx.recv() => {
                    let message = WsResponse(WebsocketResponse {
                        id: u64::MAX,
                        data: DaemonResponse::Patch(patch.data),
                    });
                    if let Err(e) = send_response(message, &mut session).await {
                        break e;
                    }
                }
                Some(Ok(msg)) = msg_stream.next() => {
                    match msg {
                        AggregatedMessage::Ping(msg) => {
                            if let Err(e) = session.pong(&msg).await {
                                error!("Failed to send Pong: {}", e);
                                break Some(CloseReason {
                                    code: CloseCode::Error,
                                    description: Some(format!("Failed to Send Pong: {}", e)),
                                });
                            };
                        }
                        AggregatedMessage::Text(msg) => {
                            match serde_json::from_slice::<WebsocketRequest>(msg.as_ref()) {
                                Ok(request) => {
                                    let request_id = request.id;
                                    let result = match local_only_reason(&request.data) {
                                        Some(reason) if !local_peer => {
                                            Ok(DaemonResponse::Error(reason.to_string()))
                                        }
                                        _ => handle_packet(request.data, &mut usb_tx).await,
                                    };
                                    let response = match result {
                                        Ok(resp) => {
                                            match resp {
                                                DaemonResponse::Ok => {
                                                    WsResponse(WebsocketResponse {
                                                        id: request_id,
                                                        data: DaemonResponse::Ok,
                                                    })
                                                }
                                                DaemonResponse::Error(error) => {
                                                    WsResponse(WebsocketResponse {
                                                        id: request_id,
                                                        data: DaemonResponse::Error(error),
                                                    })
                                                }
                                                DaemonResponse::Status(status) => {
                                                    WsResponse(WebsocketResponse {
                                                        id: request_id,
                                                        data: DaemonResponse::Status(status),
                                                    })
                                                }
                                                DaemonResponse::MicLevel(level) => {
                                                    WsResponse(WebsocketResponse {
                                                        id: request_id,
                                                        data: DaemonResponse::MicLevel(level),
                                                    })
                                                }
                                                _ => {
                                                    // This should never fucking happen
                                                    break Some(CloseReason {
                                                        code: CloseCode::Abnormal,
                                                        description: Some("Unexpected Message Type".to_string()),
                                                    });
                                                }
                                            }
                                        },
                                        Err(error) => {
                                            WsResponse(WebsocketResponse {
                                                id: request_id,
                                                data: DaemonResponse::Error(error.to_string()),
                                            })
                                        }
                                    };
                                    if let Err(e) = send_response(response, &mut session).await {
                                        break e;
                                    }
                                }
                                Err(error) => {
                                    // Ok, we weren't able to deserialise the request into a proper object, we
                                    // now need to confirm whether it was at least valid JSON with a request id
                                    warn!("Error Deserialising Request to Object: {}", error);
                                    warn!("Original Request: {}", msg);

                                    debug!("Attempting Low Level request Id Extraction..");
                                    let request: serde_json::Result<Value> = serde_json::from_str(msg.as_ref());
                                    match request {
                                        Ok(value) => {
                                            if let Some(request_id) = value["id"].as_u64() {
                                                let response = WsResponse(WebsocketResponse {
                                                    id: request_id,
                                                    data: DaemonResponse::Error(error.to_string()),
                                                });
                                                if let Err(e) = send_response(response, &mut session).await {
                                                    break e;
                                                }
                                            } else {
                                                warn!("id missing, Cannot continue. Closing connection");
                                                let error = CloseReason {
                                                    code: CloseCode::Invalid,
                                                    description: Some(String::from(
                                                        "Missing or invalid Request ID",
                                                    )),
                                                };
                                                break Some(error);
                                            }
                                        }
                                        Err(error) => {
                                            warn!("JSON structure is invalid, closing connection.");
                                            let error = CloseReason {
                                                code: CloseCode::Invalid,
                                                description: Some(error.to_string()),
                                            };
                                            break Some(error);
                                        }
                                    }
                                }
                            }
                        }
                        AggregatedMessage::Binary(_) => {
                            error!("Received Binary Message, aborting!");
                            break Some(CloseReason {
                                code: CloseCode::Unsupported,
                                description: Some("Binary is not Supported".to_string()),
                            });
                        }
                        AggregatedMessage::Pong(_) => {}
                        AggregatedMessage::Close(reason) => {
                            break reason;
                        }
                    }
                }
                else => {
                    break None;
                }
            }
        };

        let _ = session.close(close_reason).await;
    });

    Ok(response)
}

/// Serialises and sends a WsResponse to a Session
async fn send_response(res: WsResponse, session: &mut Session) -> Result<(), Option<CloseReason>> {
    match serde_json::to_string(&res) {
        Ok(text) => {
            if let Err(e) = session.text(text).await {
                error!("Failed to send response: {}", e);
                return Err(Some(CloseReason {
                    code: CloseCode::Error,
                    description: Some(e.to_string()),
                }));
            }
        }
        Err(e) => {
            error!("Failed to serialize response: {}", e);
            return Err(Some(CloseReason {
                code: CloseCode::Error,
                description: Some(format!("Serialization Error: {}", e)),
            }));
        }
    }
    Ok(())
}

// So, fun note, according to the actix manual, web::Json uses serde_json to deserialise, good
// news everybody! So do we.. :)
#[post("/api/command")]
async fn execute_command(
    request: web::Json<DaemonRequest>,
    app_data: Data<RwLock<AppData>>,
    req: HttpRequest,
) -> HttpResponse {
    if !is_local_peer(&req)
        && let Some(reason) = local_only_reason(&request.0)
    {
        return HttpResponse::Ok().json(DaemonResponse::Error(reason.to_string()));
    }

    let mut data = app_data.write().await;

    // Errors propagate weirdly in the javascript world, so send all as OK, and handle there.
    match handle_packet(request.0, &mut data.usb_tx).await {
        Ok(result) => HttpResponse::Ok().json(result),
        Err(error) => HttpResponse::Ok().json(DaemonResponse::Error(error.to_string())),
    }
}

#[get("/api/get-devices")]
async fn get_devices(app_data: Data<RwLock<AppData>>) -> HttpResponse {
    if let Ok(response) = get_status(app_data).await {
        return HttpResponse::Ok().json(&response);
    }
    HttpResponse::InternalServerError().finish()
}

#[get("/api/path")]
async fn get_path(app_data: Data<RwLock<AppData>>, req: HttpRequest) -> HttpResponse {
    let params = web::Query::<HashMap<String, String>>::from_query(req.query_string());
    if let Ok(params) = params {
        if let Some(path) = params.get("path") {
            if let Ok(status) = get_status(app_data).await {
                if let Ok(value) = serde_json::to_value(status) {
                    if let Ok(result) = value.query(path) {
                        return HttpResponse::Ok().json(result);
                    } else {
                        warn!("Invalid Path Provided..");
                    }
                } else {
                    warn!("Unable to Parse DaemonStatus..");
                }
            } else {
                warn!("Unable to Fetch Daemon Status..");
            }
        } else {
            warn!("Path Parameter Not Found..");
        }
    } else {
        warn!("Unable to Parse Parameters..");
    }

    HttpResponse::InternalServerError().finish()
}

#[get("/files/scribble/{serial}/{fader}.png")]
async fn get_scribble(
    path: web::Path<(String, FaderName)>,
    app_data: Data<RwLock<AppData>>,
) -> HttpResponse {
    let serial = &path.0;
    let fader = path.1;

    let final_width = 128;
    let final_height = 64;

    // Now we need to grab the DaemonResponse to get the layout of the scribble
    let mut data = app_data.write().await;
    let request = DaemonRequest::GetStatus;

    // Pull out the scribble configuration from the Daemon Status
    let scribble = handle_packet(request, &mut data.usb_tx)
        .await
        .ok()
        .and_then(|response| match response {
            DaemonResponse::Status(status) => status
                .mixers
                .get(serial)
                .and_then(|mixer| mixer.fader_status[fader].scribble.clone()),
            _ => None,
        });

    if scribble.is_none() {
        return HttpResponse::NotFound().finish();
    }

    let state = &data.scribble_state[fader];
    if state.scribble_config == scribble && state.png_data.is_some() {
        // If we already have the PNG data, return it.
        if let Some(png_data) = &data.scribble_state[fader].png_data {
            debug!("Returning Cached Scribble Image for {} - {}", serial, fader);
            let mime_type = ContentType(IMAGE_PNG);
            let mut builder = HttpResponse::Ok();
            builder.insert_header(mime_type);
            return builder.body(png_data.clone());
        }
    } else if let Some(scribble) = scribble {
        debug!("Building Scribble Image for {} - {}", serial, fader);
        let scribble_path = data.file_paths.icons.clone();

        let mut icon_path = None;
        if let Some(file) = &scribble.file_name {
            icon_path = Some(scribble_path.join(file));
        }

        let png = get_scribble_png(
            icon_path,
            scribble.bottom_text.clone(),
            scribble.left_text.clone(),
            scribble.inverted,
            final_width,
            final_height,
        );

        if let Ok(png) = png {
            data.scribble_state[fader] = ScribbleState {
                scribble_config: Some(scribble.clone()),
                png_data: Some(png.clone()),
            };

            debug!("Creating Image {}x{}", final_width, final_height);

            let mime_type = ContentType(IMAGE_PNG);
            let mut builder = HttpResponse::Ok();
            builder.insert_header(mime_type);
            return builder.body(png);
        }
    }

    debug!("Unable to Build Image: {} - {}", serial, fader);
    HttpResponse::NotFound().finish()
}

#[get("/files/samples/{sample}")]
async fn get_sample(sample: web::Path<String>, app_data: Data<RwLock<AppData>>) -> HttpResponse {
    // Get the Base Samples Path..
    let sample_path = {
        let data = app_data.read().await;
        data.file_paths.samples.clone()
    };

    let sample = sample.into_inner();
    let path = PathBuf::from(sample);

    if path.components().any(|part| part == Component::ParentDir) {
        // The path provided attempts to leave the samples dir, reject it.
        return HttpResponse::Forbidden().finish();
    }

    debug!("Attempting to Find {:?} in {:?}", path, sample_path);
    let file = find_file_in_path(sample_path, path);
    if let Some(path) = file {
        debug!("Found at {:?}", path);
        let mime_type = MimeGuess::from_path(path.clone()).first_or_octet_stream();
        let mut builder = HttpResponse::Ok();
        builder.insert_header(ContentType(mime_type));
        return builder.body(fs::read(path).unwrap());
    }

    HttpResponse::NotFound().finish()
}

#[post("/firmware-upload/{serial}")]
async fn upload_firmware(
    path: web::Path<String>,
    mut payload: Multipart,
    app_data: Data<RwLock<AppData>>,
) -> HttpResponse {
    let serial = path.into_inner();

    // The serial is used in the file name, so only accept characters a serial can contain.
    let serial_valid = !serial.is_empty()
        && serial.len() <= 64
        && serial
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !serial_valid {
        return firmware_error(HttpResponse::BadRequest(), "Invalid Serial");
    }

    let mut field = match payload.next().await {
        Some(Ok(field)) => field,
        Some(Err(e)) => {
            return HttpResponse::InternalServerError()
                .body(format!("Error processing multipart: {e}"));
        }
        None => return HttpResponse::BadRequest().body("No file found in request"),
    };

    // Use a unique name, and refuse to reuse an existing file (or link) at that path.
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let file_path = env::temp_dir().join(format!("goxlr-firmware-{serial}-{timestamp}.bin"));
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
    {
        Ok(f) => f,
        Err(e) => {
            return HttpResponse::InternalServerError().body(format!("Failed to create file: {e}"));
        }
    };

    let mut size = 0;
    while let Some(chunk) = field.next().await {
        let error = match chunk {
            Ok(data) => {
                size += data.len();
                if size > MAX_FIRMWARE_SIZE {
                    Some(firmware_error(
                        HttpResponse::PayloadTooLarge(),
                        "File is too large to be a firmware",
                    ))
                } else if let Err(e) = file.write_all(&data) {
                    Some(
                        HttpResponse::InternalServerError()
                            .body(format!("Failed to write file: {e}")),
                    )
                } else {
                    None
                }
            }
            Err(e) => Some(
                HttpResponse::InternalServerError().body(format!("Error reading file chunk: {e}")),
            ),
        };
        if let Some(response) = error {
            drop(file);
            let _ = fs::remove_file(&file_path);
            return response;
        }
    }
    drop(file);

    // When we get here, the file has been uploaded successfully...
    let data = app_data.read().await;
    let (tx, rx) = oneshot::channel();

    let command = DeviceCommand::RunFirmwareUpdate(serial, Some(file_path.clone()), false, tx);
    if data.usb_tx.send(command).await.is_err() {
        let _ = fs::remove_file(&file_path);
        return HttpResponse::InternalServerError().body("Unable to reach the device handler");
    }

    match rx.await {
        Ok(Ok(())) => HttpResponse::Ok().body(serde_json::to_string(&DaemonResponse::Ok).unwrap()),
        Ok(Err(e)) => {
            let _ = fs::remove_file(&file_path);
            firmware_error(HttpResponse::BadRequest(), &e.to_string())
        }
        Err(e) => HttpResponse::InternalServerError().body(format!("Error Occurred: {e}")),
    }
}

fn firmware_error(mut builder: actix_web::HttpResponseBuilder, message: &str) -> HttpResponse {
    let response = DaemonResponse::Error(message.to_string());
    builder.body(serde_json::to_string(&response).unwrap())
}

async fn default(req: HttpRequest) -> HttpResponse {
    let path = if req.path() == "/" || req.path() == "" {
        "/index.html"
    } else {
        req.path()
    };
    let path_part = &path[1..path.len()];
    let file = WEB_CONTENT.get_file(path_part);
    if let Some(file) = file {
        let mime_type = MimeGuess::from_path(path).first_or_octet_stream();
        let mut builder = HttpResponse::Ok();
        builder.insert_header(ContentType(mime_type));
        builder.body(file.contents())
    } else {
        HttpResponse::NotFound().finish()
    }
}

async fn get_status(app_data: Data<RwLock<AppData>>) -> Result<DaemonStatus> {
    let mut data = app_data.write().await;
    let request = DaemonRequest::GetStatus;

    let result = handle_packet(request, &mut data.usb_tx).await?;
    match result {
        DaemonResponse::Status(status) => Ok(status),
        _ => Err(anyhow!("Unexpected Daemon Status Result: {:?}", result)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(cors_enabled: bool) -> RequestPolicy {
        RequestPolicy {
            cors_enabled,
            host_names: vec!["localhost".into(), "0.0.0.0".into(), "my-pc".into()],
        }
    }

    #[test]
    fn hosts() {
        let policy = policy(false);
        assert!(policy.is_host_allowed("localhost:14564"));
        assert!(policy.is_host_allowed("LOCALHOST:14564"));
        assert!(policy.is_host_allowed("127.0.0.1:14564"));
        assert!(policy.is_host_allowed("[::1]:14564"));
        assert!(policy.is_host_allowed("192.168.1.20:14564"));
        assert!(policy.is_host_allowed("my-pc:14564"));
        assert!(policy.is_host_allowed("goxlr.localhost"));

        // DNS rebinding domains
        assert!(!policy.is_host_allowed("evil.com:14564"));
        assert!(!policy.is_host_allowed("localhost.evil.com:14564"));
        assert!(!policy.is_host_allowed("127.0.0.1.nip.io:14564"));
    }

    #[test]
    fn same_origin() {
        let policy = policy(false);
        let host = Some("localhost:14564");
        assert!(policy.is_origin_allowed("http://localhost:14564", host));
        assert!(policy.is_origin_allowed("http://192.168.1.20:14564", Some("192.168.1.20:14564")));

        assert!(!policy.is_origin_allowed("https://evil.com", host));
        assert!(!policy.is_origin_allowed("http://localhost.evil.com", host));
        assert!(!policy.is_origin_allowed("http://localhost:3000", host));
        assert!(!policy.is_origin_allowed("null", host));
        assert!(!policy.is_origin_allowed("file://", host));
        assert!(!policy.is_origin_allowed("http://localhost:14564", None));
    }

    #[test]
    fn cors_origins() {
        let policy = policy(true);
        let host = Some("localhost:14564");
        assert!(policy.is_origin_allowed("http://localhost:3000", host));
        assert!(policy.is_origin_allowed("http://127.0.0.1:3000", host));
        assert!(policy.is_origin_allowed("http://[::1]:3000", host));

        assert!(!policy.is_origin_allowed("http://localhost.evil.com", host));
        assert!(!policy.is_origin_allowed("http://127.0.0.1.evil.com", host));
        assert!(!policy.is_origin_allowed("http://192.168.1.20:3000", host));

        assert!(is_loopback_origin("http://localhost:8080"));
        assert!(!is_loopback_origin("http://localhost.evil.com"));
    }
}
