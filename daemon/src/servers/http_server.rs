use crate::settings::SettingsHandle;
use actix_cors::Cors;
use actix_multipart::Multipart;
use actix_web::body::MessageBody;
use actix_web::dev::{ServerHandle, ServiceRequest, ServiceResponse};
use actix_web::error::{ErrorForbidden, ErrorUnauthorized};
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
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};
use sysinfo::System;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast::Sender as BroadcastSender;
use tokio::sync::oneshot::Sender;
use tokio::sync::{RwLock, oneshot};

use crate::PatchEvent;
use crate::files::{
    FilePaths, find_file_in_path, import_sample_target, named_file_path, new_named_file_path,
};
use goxlr_ipc::{
    DaemonCommand, DaemonRequest, DaemonResponse, DaemonStatus, GoXLRCommand, HttpSettings,
    Scribble, WebsocketRequest, WebsocketResponse,
};
use goxlr_scribbles::get_scribble_png;
use goxlr_types::FaderName;

use crate::primary_worker::{DeviceCommand, DeviceSender};
use crate::servers::server_packet::handle_packet;

const WEB_CONTENT: Dir = include_dir!("$CARGO_MANIFEST_DIR/web-content/");

// Firmware files are a few megabytes, anything far beyond this isn't a firmware.
const MAX_FIRMWARE_SIZE: usize = 64 * 1024 * 1024;

// The largest audio file that can be imported as a sample.
const MAX_SAMPLE_SIZE: usize = 512 * 1024 * 1024;
// Limits for one import request, so a client can't fill the disk in one go.
const MAX_IMPORT_FILES: usize = 200;
// A profile is XML and four small screen images
const MAX_PROFILE_SIZE: usize = 16 * 1024 * 1024;
const MAX_IMPORT_TOTAL: usize = 2 * 1024 * 1024 * 1024;

// The cookie that holds the network access token, set when a link with ?token= is opened.
const ACCESS_COOKIE: &str = "goxlr_access";

/// Compares without stopping at the first difference, so the time taken doesn't tell how much
/// of a guessed token was right.
fn same_token(given: &str, token: &str) -> bool {
    given.len() == token.len()
        && given
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// The token a request presents: a `token` query parameter (a link just opened), a Bearer
/// header, or the cookie a link set earlier, in that order: an old cookie never hides the new
/// link. Returns it with whether it came from the query (the cookie is then set on the response).
fn presented_token(req: &ServiceRequest) -> Option<(String, bool)> {
    if let Some(token) = req
        .query_string()
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
    {
        return Some((token.to_string(), true));
    }
    if let Some(value) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = value.strip_prefix("Bearer ")
    {
        return Some((token.trim().to_string(), false));
    }
    req.headers()
        .get_all(header::COOKIE)
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == ACCESS_COOKIE).then(|| (value.to_string(), false))
        })
}

/// Commands a client without an origin (the Stream Deck plugin, but also any web page through
/// a sandboxed frame) may send: controlling the mixer, nothing that writes to disk. Anything
/// not listed, including commands added later, is refused.
fn allowed_without_origin(command: &GoXLRCommand) -> bool {
    matches!(
        command,
        GoXLRCommand::SetFader(..)
            | GoXLRCommand::SetFaderMuteFunction(..)
            | GoXLRCommand::SetFaderMuteState(..)
            | GoXLRCommand::SetVolume(..)
            | GoXLRCommand::SetRouter(..)
            | GoXLRCommand::SetCoughMuteFunction(..)
            | GoXLRCommand::SetCoughIsHold(..)
            | GoXLRCommand::SetCoughMuteState(..)
            | GoXLRCommand::SetSwearButtonVolume(..)
            | GoXLRCommand::SetSubMixEnabled(..)
            | GoXLRCommand::SetSubMixVolume(..)
            | GoXLRCommand::SetSubMixLinked(..)
            | GoXLRCommand::SetSubMixOutputMix(..)
            | GoXLRCommand::SetMonitorMix(..)
            | GoXLRCommand::LoadProfile(..)
            | GoXLRCommand::LoadMicProfile(..)
            | GoXLRCommand::LoadProfileColours(..)
            | GoXLRCommand::LoadEffectPreset(..)
            | GoXLRCommand::SetActiveEffectPreset(..)
            | GoXLRCommand::SetActiveSamplerBank(..)
            | GoXLRCommand::SetFXEnabled(..)
            | GoXLRCommand::SetMegaphoneEnabled(..)
            | GoXLRCommand::SetRobotEnabled(..)
            | GoXLRCommand::SetHardTuneEnabled(..)
            | GoXLRCommand::SetReverbAmount(..)
            | GoXLRCommand::SetEchoAmount(..)
            | GoXLRCommand::SetPitchAmount(..)
            | GoXLRCommand::SetGenderAmount(..)
            | GoXLRCommand::SetGateActive(..)
            | GoXLRCommand::PlaySampleByIndex(..)
            | GoXLRCommand::PlayNextSample(..)
            | GoXLRCommand::StopSamplePlayback(..)
    )
}

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

    /// An Origin is accepted if it matches the Host (the request comes from a page we served), if
    /// it's a local file, or if CORS is enabled and the page is served from this machine's loopback
    /// address.
    fn is_origin_allowed(&self, origin: &str, host: Option<&str>) -> bool {
        // Local HTML apps, such as Stream Deck plugins, connect from a file:// page. No web site
        // can send this origin ('null', which any site can produce with a sandboxed iframe, is
        // still refused).
        if origin.eq_ignore_ascii_case("file://") {
            return true;
        }
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

/// A 'null' origin: sent by some local apps, but also by sandboxed pages of any web site.
fn is_opaque_origin(origin: &str) -> bool {
    origin.eq_ignore_ascii_case("null")
}

/// Where a request comes from, which decides what it's allowed to do.
#[derive(Clone, Copy)]
struct RequestSource {
    // From this machine, rather than another machine on the network.
    local_peer: bool,
    // With a 'null' origin, which a web page can also send.
    opaque_origin: bool,
}

impl RequestSource {
    fn of(req: &HttpRequest) -> Self {
        Self {
            local_peer: req
                .peer_addr()
                .is_some_and(|addr| addr.ip().to_canonical().is_loopback()),
            opaque_origin: req
                .headers()
                .get(header::ORIGIN)
                .and_then(|o| o.to_str().ok())
                .is_some_and(is_opaque_origin),
        }
    }

    /// Why this request is refused from this source, if it is. The HTTP API has no
    /// authentication, so:
    /// - from another machine, nothing that runs a program or flashes a file from this disk,
    /// - with a 'null' origin (which any web page can send), only reading the status and
    ///   controlling the mixer, the same things the Stream Deck plugin does.
    fn refused_reason(&self, request: &DaemonRequest) -> Option<&'static str> {
        if self.opaque_origin {
            let allowed = match request {
                DaemonRequest::Ping | DaemonRequest::GetStatus | DaemonRequest::GetMicLevel(_) => {
                    true
                }
                DaemonRequest::Command(_, command) => allowed_without_origin(command),
                _ => false,
            };
            if !allowed {
                return Some("This request isn't allowed from a page without an origin");
            }
        }

        if !self.local_peer {
            match request {
                DaemonRequest::Daemon(DaemonCommand::SetActivatorPath(_)) => {
                    return Some("The activator can only be changed from this computer");
                }
                DaemonRequest::RunFirmwareUpdate(_, Some(_), _) => {
                    return Some("A firmware file on disk can only be flashed from this computer");
                }
                // Restarting is what applies a network access change: it's managed here too
                DaemonRequest::GetNetworkAccess
                | DaemonRequest::Daemon(DaemonCommand::RegenerateNetworkToken)
                | DaemonRequest::Daemon(DaemonCommand::SetAllowNetworkAccess(_))
                | DaemonRequest::Daemon(DaemonCommand::RestartDaemon) => {
                    return Some("Network access can only be managed from this computer");
                }
                _ => {}
            }
        }
        None
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
        // Some local apps (the Stream Deck plugin) send a 'null' origin, which any web page can
        // also produce (with a sandboxed iframe). Those get limited access, only on the APIs the
        // plugin uses, see restricted_reason().
        let opaque = origin.is_some_and(is_opaque_origin)
            && matches!(req.path(), "/api/websocket" | "/api/command");

        let allowed = match req.app_data::<Data<RequestPolicy>>() {
            Some(policy) => {
                host.is_none_or(|host| policy.is_host_allowed(host))
                    && (opaque
                        || origin.is_none_or(|origin| policy.is_origin_allowed(origin, host)))
            }
            None => false,
        };

        (!allowed).then(|| format!("Host: {host:?}, Origin: {origin:?}"))
    };

    if let Some(details) = rejection {
        warn!("Rejected HTTP request to {} ({})", req.path(), details);
        return Err(ErrorForbidden("Request rejected, unknown Host or Origin"));
    }

    // Other machines (network access) need the access token.
    let local = req
        .peer_addr()
        .is_some_and(|addr| addr.ip().to_canonical().is_loopback());
    let mut set_cookie = None;
    if !local {
        let token = crate::settings::network_token();
        let presented = presented_token(&req);
        let valid = match (&token, &presented) {
            (Some(token), Some((given, _))) => same_token(given, token),
            _ => false,
        };
        if !valid {
            warn!(
                "Rejected HTTP request to {} from the network without a valid access token",
                req.path()
            );
            return Err(ErrorUnauthorized(
                "This GoXLR Desk needs an access link. On the computer running it, open \
                 System > Integrations and use the network access link shown there.",
            ));
        }
        if let (Some(token), Some((_, true))) = (token, presented) {
            set_cookie = Some(format!(
                "{ACCESS_COOKIE}={token}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Strict"
            ));
        }
    }

    let mut response = next.call(req).await?;
    if let Some(cookie) = set_cookie
        && let Ok(value) = header::HeaderValue::from_str(&cookie)
    {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    Ok(response)
}

struct AppData {
    usb_tx: DeviceSender,
    broadcast_tx: BroadcastSender<PatchEvent>,
    file_paths: FilePaths,
    settings: SettingsHandle,

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
    daemon_settings: SettingsHandle,
) {
    // Imports interrupted by a crash leave hidden temporary files, remove them.
    crate::files::remove_stale_imports(&file_paths.samples);

    // Create the AppData ONCE, outside the closure
    let app_data = Data::new(RwLock::new(AppData {
        broadcast_tx: broadcast_tx.clone(),
        usb_tx: usb_tx.clone(),
        file_paths: file_paths.clone(),
        settings: daemon_settings,
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
            .service(get_schema)
            .service(get_sample)
            .service(get_scribble)
            .service(get_path)
            .service(upload_firmware)
            .service(import_samples)
            .service(export_profile)
            .service(export_backup)
            .service(restore_backup)
            .service(import_profiles)
            .default_service(web::to(default))
    })
    // Don't hold up the daemon's shutdown waiting for open connections (such as WebSockets).
    .shutdown_timeout(2)
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
    let source = RequestSource::of(&req);

    let data = app_data.read().await;
    let mut usb_tx = data.usb_tx.clone();
    let mut broadcast_rx = data.broadcast_tx.subscribe();

    // Another machine came in with the token of the moment: when access is turned off or the
    // token changes, its socket is closed (its next requests are refused anyway).
    let remote_token = (!source.local_peer).then(crate::settings::network_token);
    let mut access_check = tokio::time::interval(ACCESS_CHECK_INTERVAL);

    // Spawn the handler (this is now where we do stuff)
    actix_web::rt::spawn(async move {
        let mut msg_stream = msg_stream.aggregate_continuations();

        let close_reason = loop {
            tokio::select! {
                _ = access_check.tick(), if remote_token.is_some() => {
                    let current = crate::settings::network_token();
                    if current.is_none() || current != *remote_token.as_ref().unwrap_or(&None) {
                        break Some(CloseReason {
                            code: CloseCode::Policy,
                            description: Some(String::from("Network access was turned off or its link changed")),
                        });
                    }
                }
                received = broadcast_rx.recv() => {
                    let patch = match received {
                        Ok(patch) => patch,
                        // Patches are differences: once one is missed, the client's state is
                        // wrong. Close, the client reconnects and gets the whole status again.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                            warn!("WebSocket client missed {} updates, asking it to resynchronise", missed);
                            break Some(CloseReason {
                                code: CloseCode::Again,
                                description: Some(String::from("Missed updates, reconnect to resynchronise")),
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break None,
                    };
                    let message = WsResponse(WebsocketResponse {
                        id: u64::MAX,
                        data: DaemonResponse::Patch(patch.data),
                    });
                    if let Err(e) = send_response(message, &mut session).await {
                        break e;
                    }
                }
                msg = msg_stream.next() => {
                    // The client has gone (or the stream failed), stop handling this socket,
                    // otherwise this task lives on waiting for patches.
                    let Some(Ok(msg)) = msg else {
                        break None;
                    };
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
                                    let result = match source.refused_reason(&request.data) {
                                        Some(reason) => {
                                            Ok(DaemonResponse::Error(reason.to_string()))
                                        }
                                        None => handle_packet(request.data, &mut usb_tx).await,
                                    };
                                    let response = match result {
                                        Ok(DaemonResponse::Patch(_)) => {
                                            // Patches are only ever broadcast, never a reply.
                                            break Some(CloseReason {
                                                code: CloseCode::Abnormal,
                                                description: Some("Unexpected Message Type".to_string()),
                                            });
                                        }
                                        Ok(data) => WsResponse(WebsocketResponse { id: request_id, data }),
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
    if let Some(reason) = RequestSource::of(&req).refused_reason(&request.0) {
        return HttpResponse::Ok().json(DaemonResponse::Error(reason.to_string()));
    }

    // The sender is cloned so a slow command doesn't hold the lock every request waits on.
    let mut usb_tx = app_data.read().await.usb_tx.clone();

    // Errors propagate weirdly in the javascript world, so send all as OK, and handle there.
    match handle_packet(request.0, &mut usb_tx).await {
        Ok(result) => HttpResponse::Ok().json(result),
        Err(error) => HttpResponse::Ok().json(DaemonResponse::Error(error.to_string())),
    }
}

/// The JSON Schema of the requests /api/command accepts.
#[get("/api/schema")]
async fn get_schema() -> HttpResponse {
    static SCHEMA: LazyLock<schemars::Schema> = LazyLock::new(goxlr_ipc::request_schema);
    HttpResponse::Ok().json(&*SCHEMA)
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
    let (mut usb_tx, icons) = {
        let data = app_data.read().await;
        (data.usb_tx.clone(), data.file_paths.icons.clone())
    };
    let request = DaemonRequest::GetStatus;

    // Pull out the scribble configuration from the Daemon Status
    let scribble =
        handle_packet(request, &mut usb_tx)
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

    let cached = {
        let data = app_data.read().await;
        let state = &data.scribble_state[fader];
        (state.scribble_config == scribble)
            .then(|| state.png_data.clone())
            .flatten()
    };
    if let Some(png_data) = cached {
        // If we already have the PNG data, return it.
        debug!("Returning Cached Scribble Image for {} - {}", serial, fader);
        let mime_type = ContentType(IMAGE_PNG);
        let mut builder = HttpResponse::Ok();
        builder.insert_header(mime_type);
        return builder.body(png_data);
    } else if let Some(scribble) = scribble {
        debug!("Building Scribble Image for {} - {}", serial, fader);
        let icon_path = scribble.file_name.as_ref().map(|file| icons.join(file));

        // Drawing reads the icon and encodes a PNG, off the web server's workers
        let (bottom, left, inverted) = (
            scribble.bottom_text.clone(),
            scribble.left_text.clone(),
            scribble.inverted,
        );
        let png = web::block(move || {
            get_scribble_png(icon_path, bottom, left, inverted, final_width, final_height)
        })
        .await
        .map_err(|e| anyhow!("{e}"))
        .and_then(|result| result);

        if let Ok(png) = png {
            app_data.write().await.scribble_state[fader] = ScribbleState {
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
async fn get_sample(
    sample: web::Path<String>,
    app_data: Data<RwLock<AppData>>,
    req: HttpRequest,
) -> HttpResponse {
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
    // Searching the samples folder blocks, keep it off the workers.
    let found = web::block(move || find_file_in_path(sample_path, path)).await;
    let Ok(Some(path)) = found else {
        return HttpResponse::NotFound().finish();
    };
    debug!("Found at {:?}", path);

    // Streamed from the disk, with Range requests (seeking in a long sample). A sample can be
    // edited in place: the browser must check it's unchanged (cheap, with the ETag) before
    // playing a copy it kept, or it plays the audio from before the edit.
    match actix_files::NamedFile::open_async(&path).await {
        Ok(file) => {
            let mut response = file
                .set_content_type(MimeGuess::from_path(&path).first_or_octet_stream())
                .disable_content_disposition()
                .into_response(&req);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache"),
            );
            response
        }
        // The file may have been removed, or be unreadable, since it was found.
        Err(error) => {
            warn!("Unable to read sample {:?}: {}", path, error);
            HttpResponse::NotFound().finish()
        }
    }
}

// How often a socket from another machine checks it's still allowed
const ACCESS_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

// A backup or restore never takes this long: older temporary files were left behind
const BACKUP_TEMP_LIFETIME: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

static BACKUP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A temporary file for a backup being made or restored, in the backups folder. Files left by
/// earlier ones (Windows can't remove a file still open) are removed first, once they're old
/// enough not to belong to a backup or restore still running.
fn backup_temp_file(backups: &Path, kind: &str) -> PathBuf {
    if let Ok(entries) = fs::read_dir(backups) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let stale = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > BACKUP_TEMP_LIFETIME);
            if name.starts_with(".backup-") && name.ends_with(".zip") && stale {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let _ = fs::create_dir_all(backups);
    backups.join(format!(
        ".backup-{kind}-{}-{}.zip",
        std::process::id(),
        BACKUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// Downloads the whole configuration as a .zip ('?samples=1' adds the samples).
#[get("/files/backup")]
async fn export_backup(
    query: web::Query<HashMap<String, String>>,
    app_data: Data<RwLock<AppData>>,
) -> HttpResponse {
    let (paths, settings) = {
        let data = app_data.read().await;
        (data.file_paths.clone(), data.settings.clone())
    };
    let samples = query.get("samples").is_some_and(|value| value == "1");
    let temp = backup_temp_file(&paths.backups, "export");
    let settings = settings.backup_json().await.ok();

    let written = {
        let temp = temp.clone();
        web::block(move || -> Result<()> {
            let file = std::fs::File::create(&temp)?;
            let sink = std::io::BufWriter::new(file);
            crate::backup::write_backup(&paths, samples, settings.as_deref(), sink)
        })
        .await
    };
    if !matches!(written, Ok(Ok(()))) {
        let _ = fs::remove_file(&temp);
        return HttpResponse::InternalServerError().body("Unable to make the backup");
    }

    let Ok(file) = tokio::fs::File::open(&temp).await else {
        return HttpResponse::InternalServerError().finish();
    };
    let size = file.metadata().await.map(|m| m.len()).unwrap_or_default();
    // Already open, so it can go now on Linux and macOS (Windows: on the next backup).
    let _ = fs::remove_file(&temp);

    let date = chrono::Local::now().format("%Y-%m-%d");
    HttpResponse::Ok()
        .insert_header(ContentType(mime_guess::mime::APPLICATION_OCTET_STREAM))
        .insert_header((
            header::CONTENT_DISPOSITION,
            header::ContentDisposition::attachment(format!("goxlr-desk-backup-{date}.zip")),
        ))
        .body(actix_web::body::SizedStream::new(
            size,
            tokio_util::io::ReaderStream::new(file),
        ))
}

/// Restores a backup made by /files/backup (one .zip in a multipart upload). Replaced files
/// are kept in the backups folder.
#[post("/files/backup-restore")]
async fn restore_backup(mut payload: Multipart, app_data: Data<RwLock<AppData>>) -> HttpResponse {
    let (paths, settings, mut usb_tx) = {
        let data = app_data.read().await;
        (
            data.file_paths.clone(),
            data.settings.clone(),
            data.usb_tx.clone(),
        )
    };
    let Some(Ok(mut field)) = payload.next().await else {
        return HttpResponse::BadRequest().json(DaemonResponse::Error("No file sent".into()));
    };

    let temp = backup_temp_file(&paths.backups, "restore");
    let received: Result<()> = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        let mut size = 0;
        while let Some(chunk) = field.next().await {
            let data = chunk.map_err(|e| anyhow!("Error reading the file: {e}"))?;
            size += data.len();
            if size > MAX_IMPORT_TOTAL * 2 {
                return Err(anyhow!("The file is too large"));
            }
            file.write_all(&data).await?;
        }
        file.flush().await?;
        Ok(())
    }
    .await;
    if let Err(e) = received {
        let _ = fs::remove_file(&temp);
        return HttpResponse::Ok().json(DaemonResponse::Error(e.to_string()));
    }

    let restored = {
        let temp = temp.clone();
        web::block(
            move || -> Result<(crate::backup::RestoreSummary, Option<Vec<u8>>)> {
                crate::backup::restore_backup(&paths, std::fs::File::open(&temp)?)
            },
        )
        .await
    };
    let _ = fs::remove_file(&temp);

    match restored {
        Ok(Ok((mut summary, settings_json))) => {
            if let Some(json) = settings_json {
                match settings.restore_backup_json(&json).await {
                    Ok(()) => summary.settings = true,
                    Err(e) => {
                        return HttpResponse::Ok().json(DaemonResponse::Error(format!(
                            "The files were restored, but not the settings: {e}"
                        )));
                    }
                }
            }
            // Refresh the samples list (profiles are seen by the file watcher)
            let _ = handle_packet(
                DaemonRequest::Daemon(DaemonCommand::ApplySampleChange),
                &mut usb_tx,
            )
            .await;
            HttpResponse::Ok().json(summary)
        }
        Ok(Err(e)) => HttpResponse::Ok().json(DaemonResponse::Error(e.to_string())),
        Err(e) => HttpResponse::Ok().json(DaemonResponse::Error(e.to_string())),
    }
}

/// Profiles and mic profiles: their folder and file extension, chosen with '?kind=mic'.
fn profile_kind(
    query: &HashMap<String, String>,
    paths: &FilePaths,
) -> (PathBuf, &'static str, bool) {
    if query.get("kind").is_some_and(|kind| kind == "mic") {
        (paths.mic_profiles.clone(), "goxlrMicProfile", true)
    } else {
        (paths.profiles.clone(), "goxlr", false)
    }
}

/// Downloads a profile or mic profile file (to share it, or keep a copy elsewhere).
#[get("/files/profiles/{name}")]
async fn export_profile(
    name: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
    app_data: Data<RwLock<AppData>>,
) -> HttpResponse {
    let (directory, extension, _) = {
        let data = app_data.read().await;
        profile_kind(&query, &data.file_paths)
    };
    let name = name.into_inner();
    let Ok(path) = named_file_path(&directory, &name, extension) else {
        return HttpResponse::BadRequest().finish();
    };
    match web::block(move || fs::read(path)).await {
        Ok(Ok(contents)) => HttpResponse::Ok()
            .insert_header(ContentType::octet_stream())
            .insert_header((
                header::CONTENT_DISPOSITION,
                header::ContentDisposition::attachment(format!("{name}.{extension}")),
            ))
            .body(contents),
        _ => HttpResponse::NotFound().finish(),
    }
}

/// Imports profile or mic profile files (multipart). Each one must load as a profile; a name
/// already used gets a ' (2)' suffix.
#[post("/files/profiles-import")]
async fn import_profiles(
    query: web::Query<HashMap<String, String>>,
    mut payload: Multipart,
    app_data: Data<RwLock<AppData>>,
) -> HttpResponse {
    let (directory, extension, mic) = {
        let data = app_data.read().await;
        profile_kind(&query, &data.file_paths)
    };

    let mut imported = Vec::new();
    let mut errors = Vec::new();
    while let Some(field) = payload.next().await {
        if imported.len() + errors.len() >= MAX_IMPORT_FILES {
            errors.push(format!(
                "Only {MAX_IMPORT_FILES} files can be imported at once"
            ));
            break;
        }
        let mut field = match field {
            Ok(field) => field,
            Err(e) => {
                errors.push(format!("Error reading the upload: {e}"));
                break;
            }
        };
        let file_name = field
            .content_disposition()
            .and_then(|disposition| disposition.get_filename())
            .unwrap_or_default()
            .to_string();

        // Read it whole (it's small), then check it really is a profile before keeping it.
        let mut contents = Vec::new();
        let mut too_large = false;
        while let Some(chunk) = field.next().await {
            match chunk {
                Ok(data) if contents.len() + data.len() <= MAX_PROFILE_SIZE => {
                    contents.extend_from_slice(&data)
                }
                Ok(_) => too_large = true,
                Err(e) => {
                    errors.push(format!("{file_name}: {e}"));
                    too_large = true;
                    break;
                }
            }
        }
        if too_large {
            errors.push(format!(
                "{file_name}: the file is too large to be a profile"
            ));
            continue;
        }

        let directory = directory.clone();
        let saved = web::block(move || {
            save_imported_profile(&directory, &file_name, extension, mic, &contents)
                .map_err(|e| format!("{file_name}: {e}"))
        })
        .await;
        match saved {
            Ok(Ok(name)) => imported.push(name),
            Ok(Err(e)) => errors.push(e),
            Err(e) => errors.push(e.to_string()),
        }
    }

    if errors.is_empty() {
        HttpResponse::Ok().json(DaemonResponse::Ok)
    } else {
        HttpResponse::Ok().json(DaemonResponse::Error(errors.join("\n")))
    }
}

/// Checks an uploaded profile and writes it under a free name, returns that name.
fn save_imported_profile(
    directory: &Path,
    file_name: &str,
    extension: &str,
    mic: bool,
    contents: &[u8],
) -> Result<String> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or_default();
    let Some(stem) = base.strip_suffix(&format!(".{extension}")).or_else(|| {
        let dot = base.rfind('.')?;
        base[dot + 1..]
            .eq_ignore_ascii_case(extension)
            .then(|| &base[..dot])
    }) else {
        return Err(anyhow!("Not a .{} file", extension));
    };

    let reader = std::io::Cursor::new(contents);
    let valid = if mic {
        crate::mic_profile::MicProfileAdapter::from_reader(stem.to_string(), reader).is_ok()
    } else {
        crate::profile::ProfileAdapter::from_reader(stem.to_string(), reader).is_ok()
    };
    if !valid {
        return Err(anyhow!("The file isn't a valid profile"));
    }

    // A free name, 'Name (2)' when it's taken
    let mut name = stem.trim().to_string();
    let mut count = 2;
    while new_named_file_path(directory, &name, extension)?.exists() {
        name = format!("{} ({count})", stem.trim());
        count += 1;
    }
    let path = new_named_file_path(directory, &name, extension)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    std::io::Write::write_all(&mut file, contents)?;
    file.sync_all()?;
    Ok(name)
}

/// Imports audio files (multipart, one or more files) into a samples folder, given relative to
/// the samples directory with '?folder='. Names already used by a sample get a ' (2)' suffix.
#[post("/files/samples-import")]
async fn import_samples(
    query: web::Query<HashMap<String, String>>,
    mut payload: Multipart,
    app_data: Data<RwLock<AppData>>,
    req: HttpRequest,
) -> HttpResponse {
    let folder = query.get("folder").cloned().unwrap_or_default();
    let (base, mut usb_tx) = {
        let data = app_data.read().await;
        (data.file_paths.samples.clone(), data.usb_tx.clone())
    };

    // Refuse an import that can't fit, rather than fill the disk part way through
    let size = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if let Some(size) = size {
        let free = {
            let base = base.clone();
            web::block(move || available_space(&base))
                .await
                .ok()
                .flatten()
        };
        if let Some(free) = free
            && size + FREE_SPACE_MARGIN > free
        {
            return HttpResponse::Ok().json(DaemonResponse::Error(format!(
                "Not enough free disk space for this import ({} MB needed, {} MB free)",
                size / (1024 * 1024),
                free / (1024 * 1024)
            )));
        }
    }

    let mut imported = 0;
    let mut total = 0;
    let mut errors = Vec::new();
    while let Some(field) = payload.next().await {
        if imported + errors.len() >= MAX_IMPORT_FILES || total >= MAX_IMPORT_TOTAL {
            errors.push(format!(
                "Only {MAX_IMPORT_FILES} files or {} MB can be imported at once, import the rest separately",
                MAX_IMPORT_TOTAL / (1024 * 1024)
            ));
            break;
        }
        let mut field = match field {
            Ok(field) => field,
            Err(e) => {
                errors.push(format!("Error reading the upload: {e}"));
                break;
            }
        };
        let file_name = field
            .content_disposition()
            .and_then(|disposition| disposition.get_filename())
            .unwrap_or_default()
            .to_string();

        let target = {
            let (base, folder, file_name) = (base.clone(), folder.clone(), file_name.clone());
            web::block(move || import_sample_target(&base, &folder, &file_name))
                .await
                .unwrap_or_else(|e| Err(anyhow!("{e}")))
        };
        let result = match target {
            Ok(target) => write_imported_sample(&mut field, &target, MAX_IMPORT_TOTAL - total)
                .await
                .map(|size| total += size),
            Err(e) => {
                // Read the rest of this file, so the next one can be read.
                while let Some(Ok(_)) = field.next().await {}
                Err(e)
            }
        };
        match result {
            Ok(()) => imported += 1,
            Err(e) => errors.push(format!("{file_name}: {e}")),
        }
    }

    // Refresh the sample list.
    if imported > 0 {
        let _ = handle_packet(
            DaemonRequest::Daemon(DaemonCommand::ApplySampleChange),
            &mut usb_tx,
        )
        .await;
    }

    if errors.is_empty() {
        HttpResponse::Ok().json(DaemonResponse::Ok)
    } else {
        HttpResponse::Ok().json(DaemonResponse::Error(errors.join("\n")))
    }
}

// Space left free on the disk after an import
const FREE_SPACE_MARGIN: u64 = 100 * 1024 * 1024;

/// The free space of the disk holding a folder (the disk with the longest mount point above it).
fn available_space(path: &Path) -> Option<u64> {
    let path = path.canonicalize().ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
}

static IMPORT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes an uploaded file next to its target, then moves it into place. Returns its size.
async fn write_imported_sample(
    field: &mut actix_multipart::Field,
    target: &PathBuf,
    remaining: usize,
) -> Result<usize> {
    let Some(name) = target.file_name() else {
        return Err(anyhow!("Invalid file name"));
    };
    // A unique name, so a file left behind by a crash never blocks importing this name.
    let temp = target.with_file_name(format!(
        ".{}.{}-{}.import",
        name.to_string_lossy(),
        std::process::id(),
        IMPORT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await?;

    let mut size = 0;
    let written: Result<()> = async {
        while let Some(chunk) = field.next().await {
            let data = chunk.map_err(|e| anyhow!("Error reading the file: {e}"))?;
            size += data.len();
            if size > MAX_SAMPLE_SIZE {
                return Err(anyhow!("The file is too large"));
            }
            if size > remaining {
                return Err(anyhow!(
                    "The import is too large, import fewer files at once"
                ));
            }
            file.write_all(&data).await?;
        }
        file.sync_all().await?;
        drop(file);
        if tokio::fs::try_exists(target).await? {
            return Err(anyhow!("{} already exists", name.to_string_lossy()));
        }
        tokio::fs::rename(&temp, target).await?;
        Ok(())
    }
    .await;

    if written.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    written.map(|_| size)
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
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
        .await
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
                } else if let Err(e) = file.write_all(&data).await {
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
            let _ = tokio::fs::remove_file(&file_path).await;
            return response;
        }
    }
    if let Err(e) = file.flush().await {
        drop(file);
        let _ = tokio::fs::remove_file(&file_path).await;
        return HttpResponse::InternalServerError().body(format!("Failed to write file: {e}"));
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
    let mut usb_tx = app_data.read().await.usb_tx.clone();
    let request = DaemonRequest::GetStatus;

    let result = handle_packet(request, &mut usb_tx).await?;
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

        // Local HTML apps (Stream Deck plugins) connect from file:// pages.
        assert!(policy.is_origin_allowed("file://", host));
        assert!(!policy.is_origin_allowed("file://evil.com", host));
        assert!(!policy.is_origin_allowed("http://localhost:14564", None));
    }

    #[test]
    fn writes_to_disk_are_refused_without_origin() {
        let opaque = RequestSource {
            local_peer: true,
            opaque_origin: true,
        };
        let refused = |command: GoXLRCommand| {
            opaque
                .refused_reason(&DaemonRequest::Command("S1".into(), command))
                .is_some()
        };
        assert!(refused(GoXLRCommand::SaveProfile()));
        assert!(refused(GoXLRCommand::SaveMicProfile()));
        assert!(refused(GoXLRCommand::SaveActivePreset()));
        assert!(refused(GoXLRCommand::RenameActivePreset("x".into())));
        assert!(refused(GoXLRCommand::NewProfile("x".into())));
        assert!(refused(GoXLRCommand::DeleteProfile("x".into())));
        assert!(refused(GoXLRCommand::SetMicrophoneGain(
            goxlr_types::MicrophoneType::Dynamic,
            70
        )));
        assert!(!refused(GoXLRCommand::SetFXEnabled(true)));
        assert!(
            opaque
                .refused_reason(&DaemonRequest::GetNetworkAccess)
                .is_some()
        );
    }

    #[test]
    fn network_access_is_local_only() {
        let remote = RequestSource {
            local_peer: false,
            opaque_origin: false,
        };
        assert!(
            remote
                .refused_reason(&DaemonRequest::GetNetworkAccess)
                .is_some()
        );
        assert!(
            remote
                .refused_reason(&DaemonRequest::Daemon(
                    DaemonCommand::RegenerateNetworkToken
                ))
                .is_some()
        );
        assert!(remote.refused_reason(&DaemonRequest::GetStatus).is_none());
    }

    #[test]
    fn a_new_link_wins_over_an_old_cookie() {
        let req = actix_web::test::TestRequest::default()
            .uri("/?token=new")
            .insert_header((header::COOKIE, "goxlr_access=old"))
            .to_srv_request();
        assert_eq!(presented_token(&req), Some((String::from("new"), true)));
        let req = actix_web::test::TestRequest::default()
            .insert_header((header::COOKIE, "other=1; goxlr_access=old"))
            .to_srv_request();
        assert_eq!(presented_token(&req), Some((String::from("old"), false)));
    }

    #[test]
    fn tokens_compare() {
        assert!(same_token("abc123", "abc123"));
        assert!(!same_token("abc124", "abc123"));
        assert!(!same_token("abc12", "abc123"));
        assert!(!same_token("", "abc123"));
    }

    #[test]
    fn opaque_origin_is_restricted() {
        let opaque = RequestSource {
            local_peer: true,
            opaque_origin: true,
        };
        let serial = "S1".to_string();

        // What the Stream Deck plugin does
        assert!(opaque.refused_reason(&DaemonRequest::GetStatus).is_none());
        let volume = GoXLRCommand::SetVolume(goxlr_types::ChannelName::Mic, 100);
        assert!(
            opaque
                .refused_reason(&DaemonRequest::Command(serial.clone(), volume))
                .is_none()
        );
        let profile = GoXLRCommand::LoadProfile("Default".into(), true);
        assert!(
            opaque
                .refused_reason(&DaemonRequest::Command(serial.clone(), profile))
                .is_none()
        );

        // What a web page must not be able to do
        let activator = DaemonCommand::SetActivatorPath(Some("/bin/sh".into()));
        assert!(
            opaque
                .refused_reason(&DaemonRequest::Daemon(activator))
                .is_some()
        );
        assert!(
            opaque
                .refused_reason(&DaemonRequest::Daemon(DaemonCommand::StopDaemon))
                .is_some()
        );
        assert!(
            opaque
                .refused_reason(&DaemonRequest::RunFirmwareUpdate(
                    serial.clone(),
                    None,
                    false
                ))
                .is_some()
        );
        let delete = GoXLRCommand::DeleteProfile("Default".into());
        assert!(
            opaque
                .refused_reason(&DaemonRequest::Command(serial.clone(), delete))
                .is_some()
        );
        let shutdown = GoXLRCommand::SetShutdownCommands(vec![]);
        assert!(
            opaque
                .refused_reason(&DaemonRequest::Command(serial, shutdown))
                .is_some()
        );

        // A normal local page keeps full access
        let local = RequestSource {
            local_peer: true,
            opaque_origin: false,
        };
        let activator = DaemonCommand::SetActivatorPath(Some("/bin/sh".into()));
        assert!(
            local
                .refused_reason(&DaemonRequest::Daemon(activator))
                .is_none()
        );
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
