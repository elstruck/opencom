//! HTTP + WebSocket + SSE API, and the embedded web UI.

use crate::automation::SharedEngine;
use crate::hub::{Event, SharedHub};
use crate::model::*;
use crate::runtime::Runtime;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub hub: SharedHub,
    pub engine: SharedEngine,
    pub runtime: Option<Arc<Runtime>>,
    pub network: Option<crate::network::SharedNetwork>,
    pub started: Instant,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = match self.0 {
            StatusCode::NOT_FOUND => "NOT_FOUND",
            StatusCode::BAD_REQUEST => "BAD_REQUEST",
            StatusCode::UNAUTHORIZED => "UNAUTHORIZED",
            StatusCode::FORBIDDEN => "FORBIDDEN",
            StatusCode::BAD_GATEWAY => "REMOTE_ERROR",
            _ => "INTERNAL_ERROR",
        };
        (self.0, Json(json!({ "error": { "code": code, "message": self.1 } }))).into_response()
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
}
fn bad(msg: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.to_string())
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ---------------------------------------------------------------------------
// Generic CRUD over ShowConfig collections
// ---------------------------------------------------------------------------

pub trait HasId {
    fn id(&self) -> Uuid;
    fn name_key(&self) -> &str;
}
macro_rules! has_id {
    ($t:ty, $name:ident) => {
        impl HasId for $t {
            fn id(&self) -> Uuid {
                self.id
            }
            fn name_key(&self) -> &str {
                &self.$name
            }
        }
    };
}
has_id!(Channel, name);
has_id!(Group, name);
has_id!(Keyword, text);
has_id!(Automation, name);
has_id!(OscDestination, name);

type Accessor<T> = fn(&mut ShowConfig) -> &mut Vec<T>;

fn find_index<T: HasId>(v: &[T], key: &str) -> Option<usize> {
    if let Ok(id) = Uuid::parse_str(key) {
        if let Some(i) = v.iter().position(|x| x.id() == id) {
            return Some(i);
        }
    }
    v.iter().position(|x| x.name_key().eq_ignore_ascii_case(key))
}

fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                b.insert(k, v);
            }
        }
        (b, p) => *b = p,
    }
}

fn crud<T>(acc: Accessor<T>) -> Router<AppState>
where
    T: HasId + Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/",
            get(move |State(s): State<AppState>| async move { Json(s.hub.read(|c| acc(&mut c.clone()).clone())) }).post(
                move |State(s): State<AppState>, Json(mut body): Json<Value>| async move {
                    if let Value::Object(m) = &mut body {
                        m.entry("id").or_insert_with(|| json!(Uuid::new_v4()));
                    }
                    let item: T = serde_json::from_value(body).map_err(bad)?;
                    let out = item.clone();
                    s.hub.update(|c| acc(c).push(item));
                    Ok::<_, ApiError>((StatusCode::CREATED, Json(out)))
                },
            ),
        )
        .route(
            "/{id}",
            get(move |State(s): State<AppState>, Path(id): Path<String>| async move {
                let mut cfg = s.hub.config();
                let v = acc(&mut cfg);
                let i = find_index(v, &id).ok_or_else(|| not_found("item"))?;
                Ok::<_, ApiError>(Json(v[i].clone()))
            })
            .put(move |State(s): State<AppState>, Path(id): Path<String>, Json(patch): Json<Value>| async move {
                let mut cfg = s.hub.config();
                let v = acc(&mut cfg);
                let i = find_index(v, &id).ok_or_else(|| not_found("item"))?;
                let mut cur = serde_json::to_value(&v[i]).map_err(|e| bad(e))?;
                let keep_id = v[i].id();
                merge(&mut cur, patch);
                cur["id"] = json!(keep_id);
                let item: T = serde_json::from_value(cur).map_err(bad)?;
                let out = item.clone();
                s.hub.update(|c| {
                    if let Some(i) = acc(c).iter().position(|x| x.id() == keep_id) {
                        acc(c)[i] = item;
                    }
                });
                Ok::<_, ApiError>(Json(out))
            })
            .delete(move |State(s): State<AppState>, Path(id): Path<String>| async move {
                let mut cfg = s.hub.config();
                let i = find_index(acc(&mut cfg), &id).ok_or_else(|| not_found("item"))?;
                let rid = acc(&mut cfg)[i].id();
                s.hub.update(|c| acc(c).retain(|x| x.id() != rid));
                Ok::<_, ApiError>(StatusCode::NO_CONTENT)
            }),
        )
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/config", get(get_config).put(put_config))
        .route("/settings", get(get_settings).put(put_settings))
        .nest("/channels", crud::<Channel>(|c| &mut c.channels))
        .route("/channels/{id}/send", post(channel_send))
        .route("/channels/{id}/clear", post(channel_clear))
        .route("/channels/{id}/read", post(channel_read))
        .nest("/groups", crud::<Group>(|c| &mut c.groups))
        .nest("/keywords", crud::<Keyword>(|c| &mut c.keywords))
        .nest("/automations", crud::<Automation>(|c| &mut c.automations))
        .route("/automations/{id}/run", post(automation_run))
        .nest("/osc/destinations", crud::<OscDestination>(|c| &mut c.osc_destinations))
        .route("/osc/send", post(osc_send))
        .route("/midi/ports", get(midi_ports))
        .route("/midi/send", post(midi_send))
        .route("/audio/devices", get(audio_devices))
        .route("/network/peers", get(network_peers))
        .route("/network/channels", get(network_channels))
        .route("/models", get(models_list))
        .route("/models/{id}/download", post(model_download))
        .route("/transcript", get(transcript))
        .route("/transcript/clear", post(transcript_clear))
        .route("/transcript/read", post(transcript_read))
        .route("/transcript/export", get(transcript_export))
        .route("/alert", post(alert))
        .route("/ui/{command}", post(ui_command))
        .route("/webhooks/{*path}", post(webhook))
        .route("/events", get(ws_events))
        .route("/events/sse", get(sse_events))
        .layer(middleware::from_fn_with_state(state.clone(), auth));

    Router::new()
        .nest("/api/v1", api)
        .route("/", get(|| async { static_file("index.html") }))
        .route("/{file}", get(|Path(f): Path<String>| async move { static_file(&f) }))
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .with_state(state)
}

/// True for `localhost`, `*.localhost`, `*.local` (mDNS) and IP literals, with or without a port.
pub fn safe_host(host: &str) -> bool {
    let h = if host.starts_with('[') {
        host.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host.rsplit_once(':').map(|(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host }).unwrap_or(host)
    };
    let h = h.to_ascii_lowercase();
    h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") || h.parse::<std::net::IpAddr>().is_ok()
}

/// Without an API key, refuse requests whose Host header is a domain name. This blocks
/// DNS-rebinding attacks from web pages; real clients use an IP, `localhost` or `name.local`.
async fn host_guard(State(s): State<AppState>, headers: HeaderMap, req: Request, next: Next) -> Response {
    let has_key = s.hub.read(|c| c.settings.api_key.as_deref().map(|k| !k.is_empty()).unwrap_or(false));
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if has_key || host.is_empty() || safe_host(host) {
        next.run(req).await
    } else {
        ApiError(StatusCode::FORBIDDEN, format!("host {host:?} not allowed; set an API key to allow access by host name")).into_response()
    }
}

async fn auth(State(s): State<AppState>, headers: HeaderMap, req: Request, next: Next) -> Response {
    let Some(key) = s.hub.read(|c| c.settings.api_key.clone()).filter(|k| !k.is_empty()) else {
        return next.run(req).await;
    };
    let bearer = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::to_string);
    let query = req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("key=")).map(str::to_string));
    if bearer.as_deref() == Some(key.as_str()) || query.as_deref() == Some(key.as_str()) {
        next.run(req).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "missing or wrong API key".into()).into_response()
    }
}

// ---------------------------------------------------------------------------
// Static UI (embedded so the binary is self-contained)
// ---------------------------------------------------------------------------

fn static_file(name: &str) -> Response {
    let (body, ct): (&'static str, &str) = match name {
        "index.html" => (include_str!("../web/index.html"), "text/html; charset=utf-8"),
        "app.js" => (include_str!("../web/app.js"), "text/javascript; charset=utf-8"),
        "style.css" => (include_str!("../web/style.css"), "text/css; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn status(State(s): State<AppState>) -> Json<Value> {
    let cfg = s.hub.config();
    let channels: Vec<Value> = cfg
        .channels
        .iter()
        .map(|ch| json!({ "id": ch.id, "name": ch.name, "unread": s.hub.unread_count(ch.id) }))
        .collect();
    Json(json!({
        "app": "opencom",
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "uptimeSeconds": s.started.elapsed().as_secs_f64(),
        "show": cfg.name,
        "model": cfg.settings.stt.model,
        "recognizer": s.runtime.as_ref().and_then(|r| r.recognizer_name()),
        "errors": s.runtime.as_ref().map(|r| r.errors()).unwrap_or_default(),
        "channels": channels,
        "savePath": s.hub.save_path(),
    }))
}

async fn get_config(State(s): State<AppState>) -> Json<ShowConfig> {
    Json(s.hub.config())
}

async fn put_config(State(s): State<AppState>, Json(cfg): Json<ShowConfig>) -> Json<ShowConfig> {
    s.hub.replace_config(cfg);
    Json(s.hub.config())
}

async fn get_settings(State(s): State<AppState>) -> Json<Settings> {
    Json(s.hub.read(|c| c.settings.clone()))
}

async fn put_settings(State(s): State<AppState>, Json(patch): Json<Value>) -> ApiResult<Json<Settings>> {
    let mut cur = serde_json::to_value(s.hub.read(|c| c.settings.clone())).map_err(bad)?;
    if let (Some(stt_patch), Some(stt_cur)) = (patch.get("stt").cloned(), cur.get_mut("stt")) {
        merge(stt_cur, stt_patch);
    }
    let mut patch = patch;
    if let Value::Object(m) = &mut patch {
        m.remove("stt");
    }
    merge(&mut cur, patch);
    let settings: Settings = serde_json::from_value(cur).map_err(bad)?;
    s.hub.update(|c| c.settings = settings.clone());
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct TextBody {
    text: String,
}

async fn channel_send(State(s): State<AppState>, Path(id): Path<String>, Json(b): Json<TextBody>) -> ApiResult<Json<TranscriptEntry>> {
    let ch = s.hub.resolve_channel(&id).ok_or_else(|| not_found("channel"))?;
    if b.text.trim().is_empty() {
        return Err(bad("text is empty"));
    }
    if let ChannelSource::Remote { url, channel, key } = &ch.source {
        // The remote server owns this channel; our mirror shows the message when it comes back.
        crate::network::send_remote(url, key.as_deref(), channel, &b.text).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
        return Ok(Json(TranscriptEntry {
            id: Uuid::nil(),
            channel_id: ch.id,
            text: b.text,
            source: EntrySource::Typed,
            in_progress: false,
            seen: true,
            started_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            matched_keywords: vec![],
            translation: None,
        }));
    }
    Ok(Json(s.hub.add_entry(ch.id, b.text, EntrySource::Typed, false)))
}

async fn network_peers(State(s): State<AppState>) -> Json<Vec<crate::network::Peer>> {
    Json(s.network.as_ref().map(|n| n.peers()).unwrap_or_default())
}

#[derive(Deserialize)]
struct RemoteQuery {
    url: String,
    #[serde(default)]
    key: Option<String>,
}

async fn network_channels(Query(q): Query<RemoteQuery>) -> ApiResult<Json<Vec<crate::network::RemoteChannel>>> {
    crate::network::list_remote_channels(&q.url, q.key.as_deref()).await.map(Json).map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))
}

async fn channel_clear(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let ch = s.hub.resolve_channel(&id).ok_or_else(|| not_found("channel"))?;
    s.hub.clear_transcript(Some(ch.id));
    Ok(StatusCode::NO_CONTENT)
}

async fn channel_read(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let ch = s.hub.resolve_channel(&id).ok_or_else(|| not_found("channel"))?;
    s.hub.mark_seen(Some(ch.id));
    Ok(StatusCode::NO_CONTENT)
}

async fn automation_run(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let mut cfg = s.hub.config();
    let i = find_index(&cfg.automations, &id).ok_or_else(|| not_found("automation"))?;
    let aid = cfg.automations.swap_remove(i).id;
    s.engine.run_manual(aid)?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OscSendBody {
    #[serde(default)]
    destination_id: Option<Uuid>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    address: String,
    #[serde(default)]
    args: Vec<OscArg>,
}

async fn osc_send(State(s): State<AppState>, Json(b): Json<OscSendBody>) -> ApiResult<StatusCode> {
    let act = Action::Osc { destination_id: b.destination_id, host: b.host, port: b.port, address: b.address, args: b.args };
    s.engine.run_action(&act, &crate::template::Vars::new()).await.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn midi_ports() -> Json<Vec<crate::midi::MidiPort>> {
    Json(tokio::task::spawn_blocking(crate::midi::list_ports).await.unwrap_or_default())
}

#[derive(Deserialize)]
struct MidiSendBody {
    #[serde(default)]
    port: Option<String>,
    message: MidiMessage,
}

async fn midi_send(State(s): State<AppState>, Json(b): Json<MidiSendBody>) -> ApiResult<StatusCode> {
    s.engine.run_action(&Action::Midi { port: b.port, message: b.message }, &crate::template::Vars::new()).await.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn audio_devices() -> Json<Vec<crate::audio::InputDevice>> {
    Json(tokio::task::spawn_blocking(crate::audio::list_inputs).await.unwrap_or_default())
}

async fn models_list(State(s): State<AppState>) -> Json<Value> {
    let stt = s.hub.read(|c| c.settings.stt.clone());
    Json(json!({
        "modelsDir": crate::stt::models::models_dir(&stt),
        "vadInstalled": crate::stt::models::vad_path(&stt).exists(),
        "models": crate::stt::models::list(&stt),
    }))
}

async fn model_download(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let spec = crate::stt::catalog::find(&id).ok_or_else(|| not_found("model"))?;
    let stt = s.hub.read(|c| c.settings.stt.clone());
    let hub = s.hub.clone();
    tokio::spawn(async move {
        let h2 = hub.clone();
        let name = spec.name;
        let progress = move |done: u64, total: u64| {
            let pct = if total > 0 { done * 100 / total } else { 0 };
            h2.emit(Event::Status { component: "download".into(), ok: true, message: format!("{name}: {pct}% ({} MB)", done >> 20) });
        };
        match crate::stt::models::install(&stt, spec.id, &progress).await {
            Ok(_) => {
                hub.emit(Event::Status { component: "download".into(), ok: true, message: format!("{name}: installed") });
                hub.emit(Event::ConfigChanged);
            }
            Err(e) => hub.emit(Event::Status { component: "download".into(), ok: false, message: format!("{name}: {e:#}") }),
        }
    });
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct TranscriptQuery {
    channel: Option<String>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: Option<usize>,
    format: Option<String>,
}

fn filtered(s: &AppState, q: &TranscriptQuery) -> ApiResult<Vec<TranscriptEntry>> {
    let ch = match &q.channel {
        Some(c) => Some(s.hub.resolve_channel(c).ok_or_else(|| not_found("channel"))?.id),
        None => None,
    };
    let mut v: Vec<_> = s
        .hub
        .transcript()
        .into_iter()
        .filter(|e| ch.map(|c| c == e.channel_id).unwrap_or(true))
        .filter(|e| q.since.map(|t| e.updated_at > t).unwrap_or(true))
        .collect();
    let limit = q.limit.unwrap_or(500).min(5000);
    if v.len() > limit {
        v.drain(..v.len() - limit);
    }
    Ok(v)
}

async fn transcript(State(s): State<AppState>, Query(q): Query<TranscriptQuery>) -> ApiResult<Json<Vec<TranscriptEntry>>> {
    Ok(Json(filtered(&s, &q)?))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ChannelBody {
    #[serde(default)]
    channel_id: Option<Uuid>,
}

async fn transcript_clear(State(s): State<AppState>, body: Option<Json<ChannelBody>>) -> StatusCode {
    s.hub.clear_transcript(body.and_then(|b| b.0.channel_id));
    StatusCode::NO_CONTENT
}

async fn transcript_read(State(s): State<AppState>, body: Option<Json<ChannelBody>>) -> StatusCode {
    s.hub.mark_seen(body.and_then(|b| b.0.channel_id));
    StatusCode::NO_CONTENT
}

async fn transcript_export(State(s): State<AppState>, Query(mut q): Query<TranscriptQuery>) -> ApiResult<Response> {
    q.limit = Some(q.limit.unwrap_or(5000));
    let entries: Vec<_> = filtered(&s, &q)?.into_iter().filter(|e| !e.in_progress).collect();
    let names: std::collections::HashMap<Uuid, String> = s.hub.read(|c| c.channels.iter().map(|ch| (ch.id, ch.name.clone())).collect());
    if q.format.as_deref() == Some("json") {
        return Ok(Json(entries).into_response());
    }
    let mut out = String::new();
    for e in entries {
        let name = names.get(&e.channel_id).cloned().unwrap_or_else(|| "?".into());
        out.push_str(&format!("{} [{}] {}\n", e.started_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S"), name, e.text));
    }
    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"transcript.txt\"")],
        out,
    )
        .into_response())
}

#[derive(Deserialize)]
struct AlertBody {
    text: String,
    #[serde(default)]
    seconds: Option<f64>,
    #[serde(default)]
    color: Option<String>,
}

async fn alert(State(s): State<AppState>, Json(b): Json<AlertBody>) -> StatusCode {
    s.hub.emit(Event::Alert { text: b.text, seconds: b.seconds.unwrap_or(3.0), color: b.color });
    StatusCode::NO_CONTENT
}

async fn ui_command(State(s): State<AppState>, Path(command): Path<String>, body: Option<Json<ChannelBody>>) -> ApiResult<StatusCode> {
    let act = Action::Ui { command, channel_id: body.and_then(|b| b.0.channel_id) };
    s.engine.run_action(&act, &crate::template::Vars::new()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn webhook(State(s): State<AppState>, Path(path): Path<String>, body: String) -> Json<Value> {
    s.hub.emit(Event::WebhookReceived { path: path.clone(), body });
    Json(json!({ "received": path }))
}

async fn ws_events(State(s): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |sock| ws_loop(s, sock))
}

/// WebSocket: the server pushes every event as JSON. Clients may send
/// `{"type":"subscribe","events":["entryAdded",...]}` to filter (empty = all).
async fn ws_loop(s: AppState, mut sock: WebSocket) {
    let mut rx = s.hub.subscribe();
    let mut filter: Option<Vec<String>> = None;
    let hello = json!({ "event": "hello", "version": env!("CARGO_PKG_VERSION") });
    if sock.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let ev = match ev {
                    Ok(e) => e,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                let v = serde_json::to_value(&ev).unwrap_or_default();
                if let Some(f) = &filter {
                    let name = v.get("event").and_then(|x| x.as_str()).unwrap_or("");
                    if !f.is_empty() && !f.iter().any(|x| x == name) { continue; }
                }
                if sock.send(Message::Text(v.to_string().into())).await.is_err() { break; }
            }
            msg = sock.recv() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            if v.get("type").and_then(|x| x.as_str()) == Some("subscribe") {
                                filter = Some(v.get("events").and_then(|e| e.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default());
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn sse_events(State(s): State<AppState>, Query(q): Query<TranscriptQuery>) -> ApiResult<impl IntoResponse> {
    let ch = match &q.channel {
        Some(c) => Some(s.hub.resolve_channel(c).ok_or_else(|| not_found("channel"))?.id),
        None => None,
    };
    let rx = s.hub.subscribe();
    let stream = futures::stream::unfold(rx, move |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let keep = match &ev {
                        Event::EntryAdded { entry } | Event::EntryUpdated { entry } => ch.map(|c| c == entry.channel_id).unwrap_or(true),
                        _ => false,
                    };
                    if keep {
                        let data = serde_json::to_string(&ev).unwrap_or_default();
                        return Some((Ok::<_, std::convert::Infallible>(SseEvent::default().data(data)), rx));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::Engine;
    use crate::hub::Hub;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> (Router, SharedHub) {
        let hub = Hub::new(ShowConfig::default(), None);
        let engine = Engine::new(hub.clone());
        (router(AppState { hub: hub.clone(), engine, runtime: None, network: None, started: Instant::now() }), hub)
    }

    async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let st = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn channel_crud_and_send() {
        let (app, hub) = app();
        let (st, ch) = call(&app, "POST", "/api/v1/channels", Some(json!({"name": "Stage Left", "color": "#ff0000"}))).await;
        assert_eq!(st, StatusCode::CREATED);
        let id = ch["id"].as_str().unwrap().to_string();
        let (st, ch2) = call(&app, "PUT", &format!("/api/v1/channels/{id}"), Some(json!({"name": "SL"}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(ch2["name"], "SL");
        assert_eq!(ch2["color"], "#ff0000");
        // Lookup by name.
        let (st, _) = call(&app, "GET", "/api/v1/channels/sl", None).await;
        assert_eq!(st, StatusCode::OK);
        let (st, e) = call(&app, "POST", "/api/v1/channels/SL/send", Some(json!({"text": "standby"}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(e["source"], "typed");
        assert_eq!(hub.transcript().len(), 1);
        let (_, t) = call(&app, "GET", "/api/v1/transcript?channel=SL", None).await;
        assert_eq!(t.as_array().unwrap().len(), 1);
        let (st, _) = call(&app, "DELETE", &format!("/api/v1/channels/{id}"), None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, _) = call(&app, "GET", &format!("/api/v1/channels/{id}"), None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn automation_validation() {
        let (app, _) = app();
        let (st, _) = call(&app, "POST", "/api/v1/automations", Some(json!({"name": "bad", "trigger": {"type": "nope"}, "actions": []}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, a) = call(
            &app,
            "POST",
            "/api/v1/automations",
            Some(json!({"name": "go", "trigger": {"type": "phrase", "phrases": ["go"]}, "actions": [{"type": "alert", "text": "GO"}]})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{a}");
        let (st, _) = call(&app, "POST", "/api/v1/automations/go/run", None).await;
        assert_eq!(st, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn api_key() {
        let (app, hub) = app();
        hub.update(|c| c.settings.api_key = Some("s3cret".into()));
        let (st, _) = call(&app, "GET", "/api/v1/status", None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = call(&app, "GET", "/api/v1/status?key=s3cret", None).await;
        assert_eq!(st, StatusCode::OK);
        // The UI itself is public.
        let req = axum::http::Request::builder().uri("/").body(Body::empty()).unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), StatusCode::OK);
    }

    #[test]
    fn hosts() {
        for h in ["localhost:7430", "127.0.0.1:7430", "[::1]:7430", "192.168.1.5", "booth.local:7430", "localhost"] {
            assert!(safe_host(h), "{h}");
        }
        for h in ["evil.com", "evil.com:7430", "127.0.0.1.evil.com"] {
            assert!(!safe_host(h), "{h}");
        }
    }

    #[tokio::test]
    async fn rebinding_blocked() {
        let (app, hub) = app();
        let req = axum::http::Request::builder().uri("/api/v1/status").header("host", "evil.example:7430").body(Body::empty()).unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
        hub.update(|c| c.settings.api_key = Some("k".into()));
        let req = axum::http::Request::builder().uri("/api/v1/status?key=k").header("host", "opencom.example").body(Body::empty()).unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn settings_patch_keeps_other_fields() {
        let (app, _) = app();
        let (st, s) = call(&app, "PUT", "/api/v1/settings", Some(json!({"stt": {"threads": 4}}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(s["stt"]["threads"], 4);
        assert_eq!(s["stt"]["model"], "parakeet-tdt-0.6b-v2-int8");
        assert_eq!(s["oscListenPort"], 9431);
    }
}
