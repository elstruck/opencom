//! Runtime: turns the show configuration into running captures and pipelines,
//! and rebuilds them when audio-relevant settings change.

use crate::audio::{self, Capture};
use crate::hub::{Event, SharedHub};
use crate::model::{ChannelSource, SttSettings};
use crate::stt::{self, AudioMsg, ChannelPipeline, PipelineConfig, StreamingPipeline};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, PartialEq)]
struct Signature {
    sources: Vec<(Uuid, ChannelSource, bool)>,
    stt: SttSettings,
}

fn signature(hub: &SharedHub) -> Signature {
    hub.read(|c| Signature {
        sources: c.channels.iter().map(|ch| (ch.id, ch.source.clone(), ch.enabled)).collect(),
        stt: c.settings.stt.clone(),
    })
}

pub struct Runtime {
    hub: SharedHub,
    inner: Mutex<Inner>,
    tokio: tokio::runtime::Handle,
}

#[derive(Default)]
struct Inner {
    captures: Vec<Capture>,
    remotes: Vec<tokio::task::JoinHandle<()>>,
    pipelines: Vec<std::thread::JoinHandle<()>>,
    rec: Option<(String, stt::Engine)>,
    partial: Option<(String, std::sync::Arc<dyn stt::StreamingRecognizer>)>,
    sig: Option<Signature>,
    errors: HashMap<String, String>,
}

impl Runtime {
    pub fn new(hub: SharedHub) -> Arc<Self> {
        Arc::new(Self { hub, inner: Mutex::new(Inner::default()), tokio: tokio::runtime::Handle::current() })
    }

    /// Start captures now and keep them in sync with config changes.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.rebuild_if_changed());
        // Retry failed parts (unplugged device, model downloaded later) every 10 s.
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
            tick.tick().await;
            loop {
                tick.tick().await;
                let retry = {
                    let mut inner = me.inner.lock();
                    let has = !inner.errors.is_empty();
                    if has {
                        inner.sig = None;
                    }
                    has
                };
                if retry {
                    let m = me.clone();
                    let _ = tokio::task::spawn_blocking(move || m.rebuild_if_changed()).await;
                }
            }
        });
        let me = self.clone();
        let mut rx = self.hub.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(Event::ConfigChanged) => {
                        let m = me.clone();
                        let _ = tokio::task::spawn_blocking(move || m.rebuild_if_changed()).await;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }

    pub fn errors(&self) -> HashMap<String, String> {
        self.inner.lock().errors.clone()
    }

    pub fn recognizer_name(&self) -> Option<String> {
        self.inner.lock().rec.as_ref().map(|(_, r)| r.name())
    }

    fn status(&self, component: &str, ok: bool, message: String) {
        if ok {
            tracing::info!("{component}: {message}");
        } else {
            tracing::warn!("{component}: {message}");
        }
        self.hub.emit(Event::Status { component: component.into(), ok, message });
    }

    pub fn rebuild_if_changed(&self) {
        let sig = signature(&self.hub);
        let mut inner = self.inner.lock();
        if inner.sig.as_ref() == Some(&sig) {
            return;
        }
        // Stop everything: dropping captures ends their threads and closes senders,
        // which ends the pipelines.
        inner.captures.clear();
        for h in inner.remotes.drain(..) {
            h.abort();
        }
        for h in inner.pipelines.drain(..) {
            let _ = h.join();
        }
        inner.errors.clear();
        inner.sig = Some(sig.clone());

        // Remote (network) channels need no speech model.
        let remotes: Vec<_> = self.hub.read(|c| c.channels.iter().filter(|ch| ch.enabled && matches!(ch.source, ChannelSource::Remote { .. })).cloned().collect());
        for ch in remotes {
            if let ChannelSource::Remote { url, channel, key } = ch.source {
                let hub = self.hub.clone();
                inner.remotes.push(self.tokio.spawn(crate::network::mirror(hub, ch.id, url, channel, key)));
            }
        }

        let active: Vec<_> = self.hub.read(|c| {
            c.channels.iter().filter(|ch| ch.enabled && matches!(ch.source, ChannelSource::Device { .. } | ChannelSource::File { .. })).cloned().collect()
        });
        if active.is_empty() {
            return;
        }

        // Load (or reuse) the recognizer.
        let model_key = format!("{}|{}|{:?}", sig.stt.model, sig.stt.threads, sig.stt.models_dir);
        if inner.rec.as_ref().map(|(k, _)| k != &model_key).unwrap_or(true) {
            inner.rec = None;
            self.status("stt", true, format!("loading model {}", sig.stt.model));
            match stt::load_engine(&sig.stt) {
                Ok(r) => {
                    self.status("stt", true, format!("model ready: {}", r.name()));
                    inner.rec = Some((model_key, r));
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    inner.errors.insert("stt".into(), msg.clone());
                    self.status("stt", false, msg);
                    return;
                }
            }
        }
        let engine = inner.rec.as_ref().unwrap().1.clone();
        // Optional streaming model for live text in two-pass mode.
        let partial = match (&engine, sig.stt.partial_model.as_deref().filter(|m| !m.is_empty())) {
            (stt::Engine::Offline(_), Some(pm)) => {
                let pkey = format!("{pm}|{:?}", sig.stt.models_dir);
                if inner.partial.as_ref().map(|(k, _)| k != &pkey).unwrap_or(true) {
                    let mut pc = sig.stt.clone();
                    pc.model = pm.to_string();
                    inner.partial = match stt::load_engine(&pc) {
                        Ok(stt::Engine::Streaming(sr)) => Some((pkey, sr)),
                        Ok(_) => {
                            self.status("stt", false, format!("partial model {pm} is not a streaming model; ignoring it"));
                            None
                        }
                        Err(e) => {
                            self.status("stt", false, format!("partial model: {e:#}"));
                            None
                        }
                    };
                }
                inner.partial.as_ref().map(|(_, sr)| sr.clone())
            }
            _ => None,
        };
        let make_seg = stt::segmenter_factory(Some(stt::models::vad_path(&sig.stt)), &sig.stt);
        let pcfg = PipelineConfig { partial_interval: sig.stt.partial_interval, gate: stt::DecodeGate::for_machine(sig.stt.threads), ..Default::default() };

        let mut device_routes: HashMap<Option<String>, Vec<(u16, std::sync::mpsc::Sender<AudioMsg>)>> = HashMap::new();
        for ch in &active {
            let (tx, rx) = std::sync::mpsc::channel::<AudioMsg>();
            let hub = self.hub.clone();
            let run: Box<dyn FnOnce() + Send> = match &engine {
                stt::Engine::Offline(rec) => match &partial {
                    Some(sr) => {
                        let p = StreamingPipeline::new(ch.id, hub, sr.session(), pcfg.clone()).with_final(rec.clone());
                        Box::new(move || p.run(rx))
                    }
                    None => {
                        let p = ChannelPipeline::new(ch.id, hub, rec.clone(), make_seg(), pcfg.clone());
                        Box::new(move || p.run(rx))
                    }
                },
                stt::Engine::Streaming(sr) => {
                    let p = StreamingPipeline::new(ch.id, hub, sr.session(), pcfg.clone());
                    Box::new(move || p.run(rx))
                }
            };
            match std::thread::Builder::new().name(format!("stt-{}", ch.name)).spawn(run) {
                Ok(h) => inner.pipelines.push(h),
                Err(e) => {
                    inner.errors.insert(ch.id.to_string(), e.to_string());
                    continue;
                }
            }
            match &ch.source {
                ChannelSource::Device { device, channel_index } => device_routes.entry(device.clone()).or_default().push((*channel_index, tx)),
                ChannelSource::File { path, looped } => match audio::start_file(path, *looped, true, tx) {
                    Ok(c) => inner.captures.push(c),
                    Err(e) => {
                        let msg = format!("{}: {e:#}", ch.name);
                        inner.errors.insert(ch.id.to_string(), msg.clone());
                        self.status("audio", false, msg);
                    }
                },
                ChannelSource::None | ChannelSource::Remote { .. } => {}
            }
        }
        for (dev, routes) in device_routes {
            let ids: Vec<u16> = routes.iter().map(|r| r.0).collect();
            match audio::start_device(dev.as_deref(), routes) {
                Ok(c) => {
                    self.status("audio", true, format!("{} capturing channels {:?}", c.label, ids));
                    inner.captures.push(c);
                }
                Err(e) => {
                    let msg = format!("{}: {e:#}", dev.as_deref().unwrap_or("default input"));
                    inner.errors.insert(format!("device:{}", dev.unwrap_or_default()), msg.clone());
                    self.status("audio", false, msg);
                }
            }
        }
    }

    pub fn stop(&self) {
        let mut inner = self.inner.lock();
        inner.captures.clear();
        for h in inner.remotes.drain(..) {
            h.abort();
        }
        for h in inner.pipelines.drain(..) {
            let _ = h.join();
        }
        inner.sig = None;
    }
}
