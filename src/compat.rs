//! Optional ProdCom-compatible API adapter.
//!
//! Off by default. When `settings.compat.prodcomPort` is set, opencom serves a second HTTP API
//! on that port with the request and response shapes of the ProdCom Application API, so existing
//! integrations (for example Bitfocus Companion buttons) keep working. Only the commonly used
//! endpoints are covered. Responses use the `{ "data": ..., "meta": {...} }` envelope.
//!
//! This module is a thin translation layer over opencom's own hub and engine.

use crate::api::AppState;
use crate::hub::Event;
use crate::model::*;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

fn meta() -> Value {
    json!({ "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true), "requestId": Uuid::new_v4() })
}

fn ok(data: Value) -> Response {
    Json(json!({ "data": data, "meta": meta() })).into_response()
}

fn err(status: StatusCode, code: &str, msg: &str) -> Response {
    (status, Json(json!({ "error": { "code": code, "message": msg }, "meta": meta() }))).into_response()
}

fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "NOT_FOUND", "resource not found")
}

fn channel_json(s: &AppState, ch: &Channel) -> Value {
    json!({
        "id": ch.id,
        "name": ch.name,
        "color": ch.color,
        "sourceType": if matches!(ch.source, ChannelSource::Remote { .. }) { "network" } else { "localAudio" },
        "speechLocale": ch.language,
        "typingEnabled": ch.typing_enabled,
        "isSelected": false,
        "unreadCount": s.hub.unread_count(ch.id),
        "keywords": ch.keywords.iter().map(keyword_json).collect::<Vec<_>>(),
    })
}

fn keyword_json(k: &Keyword) -> Value {
    json!({
        "id": k.id, "text": k.text, "shouldHighlight": k.highlight, "highlightColor": k.highlight_color,
        "replacementText": k.replacement, "isSensitive": k.sensitive,
    })
}

fn group_json(g: &Group) -> Value {
    json!({ "id": g.id, "name": g.name, "channelIds": g.channel_ids, "shareOnNetwork": false })
}

fn entry_json(s: &AppState, e: &TranscriptEntry) -> Value {
    let name = s.hub.channel(e.channel_id).map(|c| c.name).unwrap_or_default();
    json!({
        "id": e.id, "channelId": e.channel_id, "channelName": name, "text": e.text,
        "source": match e.source { EntrySource::Audio => "audio", EntrySource::Typed => "typed", EntrySource::Automation => "automation" },
        "inProgress": e.in_progress, "hasBeenSeen": e.seen, "date": e.started_at,
        "completeDate": if e.in_progress { Value::Null } else { json!(e.updated_at) },
    })
}

fn automation_json(a: &Automation) -> Value {
    let trigger = match &a.trigger {
        Trigger::Phrase { phrases, channel_ids, early, .. } => json!({ "type": "message", "keywords": phrases, "channelId": channel_ids.first(), "triggerEarly": early }),
        Trigger::Osc { address } => json!({ "type": "osc", "oscTrigger": { "address": address } }),
        Trigger::Webhook { path } => json!({ "type": "httpWebhook", "path": path }),
        Trigger::Manual => json!({ "type": "manual" }),
    };
    json!({
        "id": a.id, "name": a.name, "enabled": a.enabled,
        "executionPolicy": match a.policy { ExecutionPolicy::IgnoreIfRunning => "ignoreIfRunning", _ => "cancelAndRestart" },
        "trigger": trigger, "actions": a.actions,
    })
}

fn find_channel(s: &AppState, key: &str) -> Option<Channel> {
    s.hub.resolve_channel(key)
}

fn find_automation(s: &AppState, key: &str) -> Option<Automation> {
    s.hub.read(|c| {
        let by_id = Uuid::parse_str(key).ok().and_then(|id| c.automations.iter().find(|a| a.id == id));
        by_id.or_else(|| c.automations.iter().find(|a| a.name.eq_ignore_ascii_case(key))).cloned()
    })
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/ping", get(|| async { ok(json!({ "pong": true })) }))
        .route("/api/v1/status", get(status))
        .route("/api/v1/items", get(items))
        .route("/api/v1/channels", get(channels))
        .route("/api/v1/channels/{id}", get(channel))
        .route("/api/v1/channels/{id}/send", post(channel_send))
        .route("/api/v1/channels/{id}/clear", post(channel_clear))
        .route("/api/v1/channels/{id}/mark-read", post(channel_read))
        .route("/api/v1/channels/{id}/select", post(channel_select))
        .route("/api/v1/channels/select/next", post(|s: State<AppState>| ui(s, "nextChannel")))
        .route("/api/v1/channels/select/previous", post(|s: State<AppState>| ui(s, "previousChannel")))
        .route("/api/v1/groups", get(groups))
        .route("/api/v1/transcript", get(transcript))
        .route("/api/v1/transcript/clear", post(transcript_clear))
        .route("/api/v1/transcript/mark-read", post(transcript_read))
        .route("/api/v1/keywords", get(keywords))
        .route("/api/v1/automations", get(automations))
        .route("/api/v1/automations/{id}/trigger", post(automation_trigger))
        .route("/api/v1/automations/{id}/enable", put(|s: State<AppState>, p: Path<String>| automation_enable(s, p, true)))
        .route("/api/v1/automations/{id}/disable", put(|s: State<AppState>, p: Path<String>| automation_enable(s, p, false)))
        .route("/api/v1/window/show", post(|s: State<AppState>| ui(s, "showWindow")))
        .route("/api/v1/window/hide", post(|s: State<AppState>| ui(s, "hideWindow")))
        .route("/api/v1/window/flash", post(window_flash))
        .route("/api/v1/font/increase", post(|s: State<AppState>| ui(s, "fontBigger")))
        .route("/api/v1/font/decrease", post(|s: State<AppState>| ui(s, "fontSmaller")))
        .route("/api/v1/scroll/up", post(|s: State<AppState>| ui(s, "scrollUp")))
        .route("/api/v1/scroll/down", post(|s: State<AppState>| ui(s, "scrollDown")))
        .route("/api/v1/scroll/current", post(|s: State<AppState>| ui(s, "scrollToLatest")))
        .route("/api/v1/osc/send", post(osc_send))
        .route("/api/v1/midi/send", post(midi_send))
        .route("/api/v1/webhooks/{*path}", post(webhook))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}

async fn status(State(s): State<AppState>) -> Response {
    let cfg = s.hub.config();
    ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "uptime": s.started.elapsed().as_secs_f64(),
        "activeConfigurationName": cfg.name,
        "channelCount": cfg.channels.len(),
        "apiPort": cfg.settings.compat_prodcom_port,
        "oscPort": cfg.settings.osc_listen_port,
    }))
}

async fn items(State(s): State<AppState>) -> Response {
    let cfg = s.hub.config();
    let mut v: Vec<Value> = cfg.groups.iter().map(|g| json!({ "type": "group", "id": g.id, "name": g.name, "group": group_json(g) })).collect();
    v.extend(cfg.channels.iter().map(|c| json!({ "type": "channel", "id": c.id, "name": c.name, "channel": channel_json(&s, c) })));
    ok(json!(v))
}

async fn channels(State(s): State<AppState>) -> Response {
    let cfg = s.hub.config();
    ok(json!(cfg.channels.iter().map(|c| channel_json(&s, c)).collect::<Vec<_>>()))
}

async fn channel(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    match find_channel(&s, &id) {
        Some(c) => ok(channel_json(&s, &c)),
        None => not_found(),
    }
}

#[derive(Deserialize)]
struct TextBody {
    text: String,
}

async fn channel_send(State(s): State<AppState>, Path(id): Path<String>, Json(b): Json<TextBody>) -> Response {
    let Some(c) = find_channel(&s, &id) else { return not_found() };
    if b.text.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", "text is empty");
    }
    let e = s.hub.add_entry(c.id, b.text, EntrySource::Typed, false);
    ok(entry_json(&s, &e))
}

async fn channel_clear(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(c) = find_channel(&s, &id) else { return not_found() };
    s.hub.clear_transcript(Some(c.id));
    ok(json!({ "cleared": true }))
}

async fn channel_read(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(c) = find_channel(&s, &id) else { return not_found() };
    s.hub.mark_seen(Some(c.id));
    ok(json!({ "marked": true }))
}

async fn channel_select(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(c) = find_channel(&s, &id) else { return not_found() };
    s.hub.emit(Event::Ui { command: "selectChannel".into(), channel_id: Some(c.id) });
    ok(channel_json(&s, &c))
}

async fn ui(State(s): State<AppState>, cmd: &str) -> Response {
    s.hub.emit(Event::Ui { command: cmd.into(), channel_id: None });
    ok(json!({ "ok": true }))
}

async fn groups(State(s): State<AppState>) -> Response {
    ok(json!(s.hub.read(|c| c.groups.iter().map(group_json).collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct TranscriptQuery {
    #[serde(rename = "channelId")]
    channel_id: Option<String>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn transcript(State(s): State<AppState>, Query(q): Query<TranscriptQuery>) -> Response {
    let ch = match &q.channel_id {
        Some(k) => match find_channel(&s, k) {
            Some(c) => Some(c.id),
            None => return not_found(),
        },
        None => None,
    };
    let all: Vec<_> = s
        .hub
        .transcript()
        .into_iter()
        .filter(|e| ch.map(|c| c == e.channel_id).unwrap_or(true))
        .filter(|e| q.since.map(|t| e.started_at > t).unwrap_or(true))
        .collect();
    let total = all.len();
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0);
    // Newest first, like a paginated feed.
    let page: Vec<Value> = all.iter().rev().skip(offset).take(limit).map(|e| entry_json(&s, e)).collect();
    let mut m = meta();
    m["totalCount"] = json!(total);
    m["hasMore"] = json!(offset + page.len() < total);
    Json(json!({ "data": page, "meta": m })).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ChannelBody {
    channel_id: Option<String>,
}

async fn transcript_clear(State(s): State<AppState>, body: Option<Json<ChannelBody>>) -> Response {
    let ch = body.and_then(|b| b.0.channel_id).and_then(|k| find_channel(&s, &k)).map(|c| c.id);
    s.hub.clear_transcript(ch);
    ok(json!({ "cleared": true }))
}

async fn transcript_read(State(s): State<AppState>, body: Option<Json<ChannelBody>>) -> Response {
    let ch = body.and_then(|b| b.0.channel_id).and_then(|k| find_channel(&s, &k)).map(|c| c.id);
    s.hub.mark_seen(ch);
    ok(json!({ "marked": true }))
}

async fn keywords(State(s): State<AppState>) -> Response {
    ok(json!(s.hub.read(|c| c.keywords.iter().map(keyword_json).collect::<Vec<_>>())))
}

async fn automations(State(s): State<AppState>) -> Response {
    ok(json!(s.hub.read(|c| c.automations.iter().map(automation_json).collect::<Vec<_>>())))
}

async fn automation_trigger(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(a) = find_automation(&s, &id) else { return not_found() };
    match s.engine.run_manual(a.id) {
        Ok(()) => ok(json!({ "triggered": true })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", &format!("{e:#}")),
    }
}

async fn automation_enable(State(s): State<AppState>, Path(id): Path<String>, on: bool) -> Response {
    let Some(a) = find_automation(&s, &id) else { return not_found() };
    s.hub.update(|c| {
        if let Some(x) = c.automations.iter_mut().find(|x| x.id == a.id) {
            x.enabled = on;
        }
    });
    let a = find_automation(&s, &a.id.to_string()).unwrap();
    ok(automation_json(&a))
}

#[derive(Deserialize, Default)]
struct FlashBody {
    duration: Option<f64>,
    text: Option<String>,
    repeats: Option<u32>,
}

async fn window_flash(State(s): State<AppState>, body: Option<Json<FlashBody>>) -> Response {
    let b = body.map(|b| b.0).unwrap_or_default();
    let secs = b.duration.unwrap_or(2.0) * b.repeats.unwrap_or(1).max(1) as f64;
    s.hub.emit(Event::Alert { text: b.text.unwrap_or_else(|| "ATTENTION".into()), seconds: secs, color: None });
    ok(json!({ "ok": true }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OscBody {
    destination_id: Option<String>,
    address_pattern: String,
    #[serde(default)]
    arguments: Vec<OscArg>,
}

async fn osc_send(State(s): State<AppState>, Json(b): Json<OscBody>) -> Response {
    let dest = match b.destination_id.as_deref() {
        Some(k) => match s.hub.read(|c| {
            c.osc_destinations.iter().find(|d| d.id.to_string() == k || d.name.eq_ignore_ascii_case(k)).map(|d| d.id)
        }) {
            Some(id) => Some(id),
            None => return not_found(),
        },
        None => s.hub.read(|c| c.osc_destinations.iter().find(|d| d.enabled).map(|d| d.id)),
    };
    let Some(dest) = dest else { return err(StatusCode::BAD_REQUEST, "BAD_REQUEST", "no OSC destination configured") };
    let act = Action::Osc { destination_id: Some(dest), host: None, port: None, address: b.address_pattern, args: b.arguments };
    match s.engine.run_action(&act, &crate::template::Vars::new()).await {
        Ok(()) => ok(json!({ "sent": true })),
        Err(e) => err(StatusCode::BAD_REQUEST, "BAD_REQUEST", &format!("{e:#}")),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MidiBody {
    device_id: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    channel: u8,
    note: Option<u8>,
    velocity: Option<u8>,
    controller: Option<u8>,
    value: Option<u8>,
    program: Option<u8>,
}

async fn midi_send(State(s): State<AppState>, Json(b): Json<MidiBody>) -> Response {
    let message = match b.kind.as_str() {
        "noteOn" => MidiMessage::NoteOn { channel: b.channel, note: b.note.unwrap_or(60), velocity: b.velocity.unwrap_or(127) },
        "noteOff" => MidiMessage::NoteOff { channel: b.channel, note: b.note.unwrap_or(60), velocity: b.velocity.unwrap_or(0) },
        "controlChange" => MidiMessage::ControlChange { channel: b.channel, controller: b.controller.unwrap_or(0), value: b.value.unwrap_or(0) },
        "programChange" => MidiMessage::ProgramChange { channel: b.channel, program: b.program.unwrap_or(0) },
        _ => return err(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", "unknown MIDI type"),
    };
    match s.engine.run_action(&Action::Midi { port: b.device_id, message }, &crate::template::Vars::new()).await {
        Ok(()) => ok(json!({ "sent": true })),
        Err(e) => err(StatusCode::BAD_REQUEST, "BAD_REQUEST", &format!("{e:#}")),
    }
}

async fn webhook(State(s): State<AppState>, Path(path): Path<String>, body: String) -> Response {
    s.hub.emit(Event::WebhookReceived { path: path.clone(), body });
    ok(json!({ "triggered": path }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::Engine;
    use crate::hub::Hub;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::time::Instant;
    use tower::ServiceExt;

    #[tokio::test]
    async fn envelope_and_send() {
        let mut cfg = ShowConfig::default();
        cfg.channels.push(Channel::new("Stage Left"));
        let hub = Hub::new(cfg, None);
        let app = router(AppState { hub: hub.clone(), engine: Engine::new(hub.clone()), runtime: None, network: None, started: Instant::now() });
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/channels/stage%20left/send")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"text":"standby"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["data"]["source"], "typed");
        assert!(v["meta"]["timestamp"].is_string());

        let req = axum::http::Request::builder().uri("/api/v1/transcript?limit=10").body(Body::empty()).unwrap();
        let v: Value = serde_json::from_slice(&app.oneshot(req).await.unwrap().into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["meta"]["totalCount"], 1);
        assert_eq!(v["data"][0]["channelName"], "Stage Left");
    }
}
