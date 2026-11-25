//! Runtime: turns the show configuration into running captures and pipelines,
//! and rebuilds them when audio-relevant settings change.

use crate::audio::{self, Capture};
use crate::hub::{Event, SharedHub};
use crate::model::{ChannelSource, SttSettings};
use crate::stt::{self, AudioMsg, ChannelPipeline, PipelineConfig, SharedRecognizer};
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
}

#[derive(Default)]
struct Inner {
    captures: Vec<Capture>,
    pipelines: Vec<std::thread::JoinHandle<()>>,
    rec: Option<(String, SharedRecognizer)>,
    sig: Option<Signature>,
    errors: HashMap<String, String>,
}

impl Runtime {
    pub fn new(hub: SharedHub) -> Arc<Self> {
        Arc::new(Self { hub, inner: Mutex::new(Inner::default()) })
    }

    /// Start captures now and keep them in sync with config changes.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.rebuild_if_changed());
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
        for h in inner.pipelines.drain(..) {
            let _ = h.join();
        }
        inner.errors.clear();
        inner.sig = Some(sig.clone());

        let active: Vec<_> = self.hub.read(|c| c.channels.iter().filter(|ch| ch.enabled && ch.source != ChannelSource::None).cloned().collect());
        if active.is_empty() {
            return;
        }

        // Load (or reuse) the recognizer.
        let model_key = format!("{}|{}|{:?}", sig.stt.model, sig.stt.threads, sig.stt.models_dir);
        if inner.rec.as_ref().map(|(k, _)| k != &model_key).unwrap_or(true) {
            inner.rec = None;
            self.status("stt", true, format!("loading model {}", sig.stt.model));
            match stt::load_recognizer(&sig.stt) {
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
        let rec = inner.rec.as_ref().unwrap().1.clone();
        let make_seg = stt::segmenter_factory(Some(stt::models::vad_path(&sig.stt)), &sig.stt);
        let pcfg = PipelineConfig { partial_interval: sig.stt.partial_interval, gate: stt::DecodeGate::for_machine(sig.stt.threads), ..Default::default() };

        let mut device_routes: HashMap<Option<String>, Vec<(u16, std::sync::mpsc::Sender<AudioMsg>)>> = HashMap::new();
        for ch in &active {
            let (tx, rx) = std::sync::mpsc::channel::<AudioMsg>();
            let p = ChannelPipeline::new(ch.id, self.hub.clone(), rec.clone(), make_seg(), pcfg.clone());
            match std::thread::Builder::new().name(format!("stt-{}", ch.name)).spawn(move || p.run(rx)) {
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
                ChannelSource::None => {}
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
        for h in inner.pipelines.drain(..) {
            let _ = h.join();
        }
        inner.sig = None;
    }
}
