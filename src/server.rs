//! Start the whole app (hub, automation engine, audio runtime, OSC input, HTTP server).
//! Used by the CLI and by the desktop shell.

use crate::api::{router, AppState};
use crate::automation::Engine;
use crate::hub::{Event, Hub, SharedHub};
use crate::runtime::Runtime;
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub fn default_show_path() -> PathBuf {
    directories::ProjectDirs::from("io", "opencom", "opencom")
        .map(|d| d.config_dir().join("show.json"))
        .unwrap_or_else(|| PathBuf::from("show.json"))
}

pub struct Server {
    pub hub: SharedHub,
    pub runtime: Arc<Runtime>,
    pub addr: SocketAddr,
    pub network: crate::network::SharedNetwork,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    pub fn url(&self) -> String {
        let host = if self.addr.ip().is_unspecified() { "127.0.0.1".to_string() } else { self.addr.ip().to_string() };
        format!("http://{host}:{}/", self.addr.port())
    }

    /// Stop the HTTP server and audio, and save the show.
    pub async fn stop(mut self) -> Result<()> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
        self.network.stop();
        let rt = self.runtime.clone();
        tokio::task::spawn_blocking(move || rt.stop()).await?;
        self.hub.save()
    }
}

/// Start everything. `port` 0 picks a free port.
pub async fn start(show: PathBuf, host: &str, port: u16) -> Result<Server> {
    let hub = Hub::open(show.clone())?;
    tracing::info!("show file: {}", show.display());
    if hub.read(|c| c.channels.is_empty()) {
        hub.update(|c| c.channels.push(crate::model::Channel::new("Comms")));
    }
    let stt_cfg = hub.read(|c| c.settings.stt.clone());
    if let Err(e) = crate::stt::models::ensure_vad(&stt_cfg).await {
        tracing::warn!("could not fetch Silero VAD model ({e:#}); using energy VAD");
    }
    let engine = Engine::new(hub.clone());
    engine.start();
    let runtime = Runtime::new(hub.clone());
    runtime.start();
    if let Some(p) = hub.read(|c| c.settings.osc_listen_port) {
        let h = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::osc::listen(h.clone(), p).await {
                tracing::warn!("OSC input disabled: {e:#}");
                h.emit(Event::Status { component: "osc".into(), ok: false, message: format!("{e:#}") });
            }
        });
    }
    let listener = tokio::net::TcpListener::bind((host, port)).await.with_context(|| format!("binding {host}:{port}"))?;
    let addr = listener.local_addr()?;
    let network = crate::network::Network::new();
    let advertise = !addr.ip().is_loopback();
    if let Err(e) = network.start(addr.port(), &hub.read(|c| c.name.clone()), advertise) {
        tracing::warn!("LAN discovery disabled: {e:#}");
    }
    let app = router(AppState { hub: hub.clone(), engine, runtime: Some(runtime.clone()), network: Some(network.clone()), started: Instant::now() });
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
        {
            tracing::error!("HTTP server stopped: {e}");
        }
    });
    Ok(Server { hub, runtime, addr, network, shutdown: Some(tx), task: Some(task) })
}
