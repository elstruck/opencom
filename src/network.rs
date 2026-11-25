//! LAN features: mDNS advertising and discovery of other opencom servers, and
//! "remote" channels that mirror a channel from another opencom server.

use crate::hub::SharedHub;
use crate::model::EntrySource;
use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

pub const SERVICE_TYPE: &str = "_opencom._tcp.local.";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub name: String,
    pub show: String,
    pub url: String,
    pub version: String,
}

#[derive(Default)]
pub struct Network {
    daemon: Mutex<Option<mdns_sd::ServiceDaemon>>,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
}

pub type SharedNetwork = Arc<Network>;

impl Network {
    pub fn new() -> SharedNetwork {
        Arc::new(Self::default())
    }

    pub fn peers(&self) -> Vec<Peer> {
        let mut v: Vec<_> = self.peers.lock().values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Start mDNS: browse for peers always; advertise this server when `advertise` is true
    /// (only useful when the HTTP server listens on a LAN address).
    pub fn start(&self, port: u16, show_name: &str, advertise: bool) -> Result<()> {
        let daemon = mdns_sd::ServiceDaemon::new().context("starting mDNS")?;
        let host = hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or_else(|_| "opencom".into());
        let instance = format!("opencom on {host}");
        if advertise {
            let props = [("version", env!("CARGO_PKG_VERSION")), ("show", show_name)];
            let info = mdns_sd::ServiceInfo::new(SERVICE_TYPE, &instance, &format!("{host}.local."), "", port, &props[..])?.enable_addr_auto();
            daemon.register(info)?;
            tracing::info!("advertising {instance} on the LAN (port {port})");
        }
        let rx = daemon.browse(SERVICE_TYPE)?;
        let peers = self.peers.clone();
        let own = if advertise { Some(instance.clone()) } else { None };
        std::thread::Builder::new().name("mdns-browse".into()).spawn(move || {
            while let Ok(ev) = rx.recv() {
                match ev {
                    mdns_sd::ServiceEvent::ServiceResolved(info) => {
                        let name = info.get_fullname().trim_end_matches(SERVICE_TYPE).trim_end_matches('.').replace("\\032", " ").to_string();
                        if own.as_deref() == Some(name.as_str()) {
                            continue;
                        }
                        let Some(ip) = info.get_addresses_v4().into_iter().next() else { continue };
                        peers.lock().insert(
                            info.get_fullname().to_string(),
                            Peer {
                                name,
                                show: info.get_property_val_str("show").unwrap_or("").to_string(),
                                version: info.get_property_val_str("version").unwrap_or("").to_string(),
                                url: format!("http://{ip}:{}", info.get_port()),
                            },
                        );
                    }
                    mdns_sd::ServiceEvent::ServiceRemoved(_, full) => {
                        peers.lock().remove(&full);
                    }
                    _ => {}
                }
            }
        })?;
        *self.daemon.lock() = Some(daemon);
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(d) = self.daemon.lock().take() {
            let _ = d.shutdown();
        }
    }
}

// ---------------------------------------------------------------------------
// Remote channels
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteChannel {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

fn client(key: Option<&str>) -> Result<reqwest::Client> {
    let mut h = reqwest::header::HeaderMap::new();
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {k}").parse()?);
    }
    Ok(reqwest::Client::builder().default_headers(h).timeout(Duration::from_secs(5)).build()?)
}

fn base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

pub async fn list_remote_channels(url: &str, key: Option<&str>) -> Result<Vec<RemoteChannel>> {
    let r = client(key)?.get(format!("{}/api/v1/channels", base(url))).send().await?.error_for_status()?;
    Ok(r.json().await?)
}

/// Post a typed message to a channel on another opencom server.
pub async fn send_remote(url: &str, key: Option<&str>, channel: &str, text: &str) -> Result<()> {
    client(key)?
        .post(format!("{}/api/v1/channels/{}/send", base(url), urlencode(channel)))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteEntry {
    id: Uuid,
    channel_id: Uuid,
    text: String,
    source: EntrySource,
    in_progress: bool,
}

/// Mirror one remote channel into the local channel `local_id`. Runs until aborted,
/// reconnecting with backoff.
pub async fn mirror(hub: SharedHub, local_id: Uuid, url: String, channel: String, key: Option<String>) {
    let mut backoff = 1u64;
    let mut map: HashMap<Uuid, Uuid> = HashMap::new();
    loop {
        match mirror_once(&hub, local_id, &url, &channel, key.as_deref(), &mut map).await {
            Ok(()) => backoff = 1,
            Err(e) => {
                let name = hub.channel(local_id).map(|c| c.name).unwrap_or_default();
                tracing::warn!("remote channel {name}: {e:#}");
                hub.emit(crate::hub::Event::Status { component: "network".into(), ok: false, message: format!("{name}: {e:#} (retrying)") });
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn mirror_once(hub: &SharedHub, local_id: Uuid, url: &str, channel: &str, key: Option<&str>, map: &mut HashMap<Uuid, Uuid>) -> Result<()> {
    let chans = list_remote_channels(url, key).await?;
    let remote = chans
        .iter()
        .find(|c| c.id.to_string() == channel || c.name.eq_ignore_ascii_case(channel))
        .ok_or_else(|| anyhow!("channel {channel:?} not found on {url}"))?
        .clone();

    let mut ws_url = base(url).replacen("http", "ws", 1) + "/api/v1/events";
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        ws_url += &format!("?key={}", urlencode(k));
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str()).await.context("connecting WebSocket")?;
    let sub = serde_json::json!({ "type": "subscribe", "events": ["entryAdded", "entryUpdated", "entryRemoved"] });
    ws.send(tokio_tungstenite::tungstenite::Message::Text(sub.to_string().into())).await?;
    let name = hub.channel(local_id).map(|c| c.name).unwrap_or_default();
    hub.emit(crate::hub::Event::Status { component: "network".into(), ok: true, message: format!("{name}: mirroring {} from {url}", remote.name) });

    let mut ping = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = ping.tick() => {
                ws.send(tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into())).await?;
            }
            msg = ws.next() => {
                let Some(msg) = msg else { return Err(anyhow!("connection closed")) };
                let msg = msg?;
                let tokio_tungstenite::tungstenite::Message::Text(t) = msg else { continue };
                let v: serde_json::Value = match serde_json::from_str(&t) { Ok(v) => v, Err(_) => continue };
                match v.get("event").and_then(|e| e.as_str()) {
                    Some("entryAdded") | Some("entryUpdated") => {
                        let Ok(e) = serde_json::from_value::<RemoteEntry>(v["entry"].clone()) else { continue };
                        if e.channel_id != remote.id { continue; }
                        match map.get(&e.id) {
                            Some(local) => { hub.update_entry(*local, e.text, e.in_progress); }
                            None => {
                                let le = hub.add_entry(local_id, e.text, e.source, e.in_progress);
                                map.insert(e.id, le.id);
                                if map.len() > 5000 { map.clear(); }
                            }
                        }
                    }
                    Some("entryRemoved") => {
                        if let Some(id) = v.get("id").and_then(|x| x.as_str()).and_then(|s| Uuid::parse_str(s).ok()) {
                            if let Some(local) = map.remove(&id) { hub.remove_entry(local); }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
