//! Automation engine: watches events, matches triggers, runs action lists.

use crate::hub::{Event, SharedHub};
use crate::keywords;
use crate::model::*;
use crate::template::Vars;
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use uuid::Uuid;

struct RunState {
    handles: Vec<JoinHandle<()>>,
    last_start: Option<Instant>,
}

pub struct Engine {
    hub: SharedHub,
    runs: Mutex<HashMap<Uuid, RunState>>,
    /// (automation, entry) pairs that already fired, so one entry fires an automation once.
    fired: Mutex<(HashSet<(Uuid, Uuid)>, VecDeque<(Uuid, Uuid)>)>,
    http: reqwest::Client,
}

pub type SharedEngine = Arc<Engine>;

impl Engine {
    pub fn new(hub: SharedHub) -> SharedEngine {
        Arc::new(Self {
            hub,
            runs: Mutex::new(HashMap::new()),
            fired: Mutex::new((HashSet::new(), VecDeque::new())),
            http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("http client"),
        })
    }

    /// Spawn the event loop.
    pub fn start(self: &SharedEngine) -> JoinHandle<()> {
        let me = self.clone();
        let mut rx = self.hub.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => me.on_event(&ev),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => tracing::warn!("automation engine lagged {n} events"),
                    Err(_) => break,
                }
            }
        })
    }

    fn on_event(self: &SharedEngine, ev: &Event) {
        let automations = self.hub.read(|c| c.automations.clone());
        match ev {
            Event::EntryAdded { entry } | Event::EntryUpdated { entry } => {
                // Never react to our own messages; that would allow loops.
                if entry.source == EntrySource::Automation || entry.text.trim().is_empty() {
                    return;
                }
                let channel_name = self.hub.channel(entry.channel_id).map(|c| c.name).unwrap_or_default();
                for a in automations.iter().filter(|a| a.enabled) {
                    if let Trigger::Phrase { phrases, channel_ids, early, whole_word } = &a.trigger {
                        if entry.in_progress && !early {
                            continue;
                        }
                        if !channel_ids.is_empty() && !channel_ids.contains(&entry.channel_id) {
                            continue;
                        }
                        let Some(hit) = phrases.iter().find(|p| keywords::contains(&entry.text, p, *whole_word)) else { continue };
                        if !self.mark_fired(a.id, entry.id) {
                            continue;
                        }
                        let vars = Vars::new()
                            .set("text", entry.text.clone())
                            .set("phrase", hit.clone())
                            .set("channel", channel_name.clone())
                            .set("channelId", entry.channel_id.to_string())
                            .set("entryId", entry.id.to_string());
                        self.fire(a, vars, format!("phrase \"{hit}\" on {channel_name}"));
                    }
                }
            }
            Event::OscReceived { address, args, .. } => {
                for a in automations.iter().filter(|a| a.enabled) {
                    if let Trigger::Osc { address: pat } = &a.trigger {
                        if crate::osc::address_matches(pat, address) {
                            let mut vars = Vars::new().set("address", address.clone()).set("text", args.join(" "));
                            for (i, v) in args.iter().enumerate() {
                                vars.insert(&format!("arg{i}"), v.clone());
                            }
                            self.fire(a, vars, format!("OSC {address}"));
                        }
                    }
                }
            }
            Event::WebhookReceived { path, body } => {
                for a in automations.iter().filter(|a| a.enabled) {
                    if let Trigger::Webhook { path: p } = &a.trigger {
                        if p.trim_matches('/') == path.trim_matches('/') {
                            self.fire(a, Vars::new().set("text", body.clone()).set("path", path.clone()), format!("webhook {path}"));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn mark_fired(&self, automation: Uuid, entry: Uuid) -> bool {
        let mut f = self.fired.lock();
        if !f.0.insert((automation, entry)) {
            return false;
        }
        f.1.push_back((automation, entry));
        while f.1.len() > 10_000 {
            if let Some(old) = f.1.pop_front() {
                f.0.remove(&old);
            }
        }
        true
    }

    /// Run an automation now (manual trigger from the API or UI).
    pub fn run_manual(self: &SharedEngine, id: Uuid) -> Result<()> {
        let a = self.hub.read(|c| c.automations.iter().find(|a| a.id == id).cloned()).ok_or_else(|| anyhow!("no automation {id}"))?;
        self.fire(&a, Vars::new().set("text", ""), "manual".into());
        Ok(())
    }

    fn fire(self: &SharedEngine, a: &Automation, vars: Vars, cause: String) {
        let mut runs = self.runs.lock();
        let st = runs.entry(a.id).or_insert(RunState { handles: vec![], last_start: None });
        st.handles.retain(|h| !h.is_finished());
        if a.cooldown_seconds > 0.0 {
            if let Some(t) = st.last_start {
                if t.elapsed().as_secs_f64() < a.cooldown_seconds {
                    tracing::debug!("automation {} in cooldown", a.name);
                    return;
                }
            }
        }
        match a.policy {
            ExecutionPolicy::IgnoreIfRunning if !st.handles.is_empty() => return,
            ExecutionPolicy::CancelAndRestart => st.handles.drain(..).for_each(|h| h.abort()),
            _ => {}
        }
        st.last_start = Some(Instant::now());
        tracing::info!("automation \"{}\" fired ({cause})", a.name);
        self.hub.emit(Event::AutomationFired { automation_id: a.id, name: a.name.clone(), cause });
        let me = self.clone();
        let actions = a.actions.clone();
        let name = a.name.clone();
        st.handles.push(tokio::spawn(async move {
            for act in actions {
                if let Err(e) = me.run_action(&act, &vars).await {
                    tracing::warn!("automation \"{name}\": {e:#}");
                    me.hub.emit(Event::Status { component: "automation".into(), ok: false, message: format!("{name}: {e:#}") });
                }
            }
        }));
    }

    pub async fn run_action(&self, act: &Action, vars: &Vars) -> Result<()> {
        match act {
            Action::Delay { seconds } => {
                tokio::time::sleep(Duration::from_secs_f64(seconds.max(0.0))).await;
            }
            Action::Osc { destination_id, host, port, address, args } => {
                let (h, p) = match destination_id {
                    Some(id) => {
                        let d = self
                            .hub
                            .read(|c| c.osc_destinations.iter().find(|d| d.id == *id).cloned())
                            .ok_or_else(|| anyhow!("unknown OSC destination {id}"))?;
                        if !d.enabled {
                            return Ok(());
                        }
                        (d.host, d.port)
                    }
                    None => (
                        host.clone().ok_or_else(|| anyhow!("OSC action needs a destination or host"))?,
                        port.ok_or_else(|| anyhow!("OSC action needs a port"))?,
                    ),
                };
                let built = crate::osc::build_args(args, vars)?;
                crate::osc::send(&h, p, &vars.expand(address), built).await?;
            }
            Action::Midi { port, message } => {
                let port = port.clone();
                let message = message.clone();
                tokio::task::spawn_blocking(move || crate::midi::send(port.as_deref(), &message)).await??;
            }
            Action::Http { method, url, headers, body } => {
                let m = reqwest::Method::from_bytes(method.to_ascii_uppercase().as_bytes())?;
                let mut req = self.http.request(m, vars.expand(url));
                for (k, v) in headers {
                    req = req.header(k, vars.expand(v));
                }
                if let Some(b) = body {
                    req = req.body(vars.expand(b));
                }
                let resp = req.send().await?;
                if !resp.status().is_success() {
                    return Err(anyhow!("HTTP {}", resp.status()));
                }
            }
            Action::SendText { channel_id, text } => {
                self.hub.add_entry(*channel_id, vars.expand(text), EntrySource::Automation, false);
            }
            Action::Alert { text, seconds, color } => {
                self.hub.emit(Event::Alert { text: vars.expand(text), seconds: *seconds, color: color.clone() });
            }
            Action::Ui { command, channel_id } => {
                match command.as_str() {
                    "clearTranscript" => self.hub.clear_transcript(*channel_id),
                    "markRead" => self.hub.mark_seen(*channel_id),
                    _ => {}
                }
                self.hub.emit(Event::Ui { command: command.clone(), channel_id: *channel_id });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Hub;
    use tokio::net::UdpSocket;

    fn phrase_auto(phrase: &str, early: bool, actions: Vec<Action>) -> Automation {
        Automation {
            id: Uuid::new_v4(),
            name: format!("on {phrase}"),
            enabled: true,
            policy: ExecutionPolicy::RunParallel,
            cooldown_seconds: 0.0,
            trigger: Trigger::Phrase { phrases: vec![phrase.into()], channel_ids: vec![], early, whole_word: true },
            actions,
        }
    }

    #[tokio::test]
    async fn phrase_fires_osc_once_per_entry() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = rx.local_addr().unwrap().port();
        let mut cfg = ShowConfig::default();
        let ch = Channel::new("SM");
        let ch_id = ch.id;
        cfg.channels.push(ch);
        cfg.automations.push(phrase_auto(
            "go",
            false,
            vec![Action::Osc {
                destination_id: None,
                host: Some("127.0.0.1".into()),
                port: Some(port),
                address: "/cue/go".into(),
                args: vec![OscArg { kind: OscArgKind::String, value: "{channel}: {text}".into() }],
            }],
        ));
        let hub = Hub::new(cfg, None);
        let eng = Engine::new(hub.clone());
        eng.start();
        tokio::task::yield_now().await;

        let e = hub.add_entry(ch_id, "lights".into(), EntrySource::Audio, true);
        hub.update_entry(e.id, "lights go".into(), true); // partial: not early, ignore
        hub.update_entry(e.id, "lights go".into(), false); // final: fire
        hub.update_entry(e.id, "lights go".into(), false); // same entry: no second fire

        let mut buf = [0u8; 1024];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), rx.recv_from(&mut buf)).await.unwrap().unwrap();
        let (_, p) = rosc::decoder::decode_udp(&buf[..n]).unwrap();
        let rosc::OscPacket::Message(m) = p else { panic!() };
        assert_eq!(m.addr, "/cue/go");
        assert_eq!(m.args, vec![rosc::OscType::String("SM: lights go".into())]);
        assert!(tokio::time::timeout(Duration::from_millis(300), rx.recv_from(&mut buf)).await.is_err(), "fired twice");
    }

    #[tokio::test]
    async fn early_trigger_and_no_self_loop() {
        let mut cfg = ShowConfig::default();
        let ch = Channel::new("SM");
        let ch_id = ch.id;
        cfg.channels.push(ch);
        // "go" posts text containing "go" again; must not loop.
        cfg.automations.push(phrase_auto("go", true, vec![Action::SendText { channel_id: ch_id, text: "go received".into() }]));
        let hub = Hub::new(cfg, None);
        let mut ev = hub.subscribe();
        let eng = Engine::new(hub.clone());
        eng.start();
        tokio::task::yield_now().await;
        hub.add_entry(ch_id, "and go".into(), EntrySource::Audio, true);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut fired = 0;
        while let Ok(e) = ev.try_recv() {
            if matches!(e, Event::AutomationFired { .. }) {
                fired += 1;
            }
        }
        assert_eq!(fired, 1);
        assert_eq!(hub.transcript().len(), 2);
    }

    #[tokio::test]
    async fn ignore_if_running_and_cancel() {
        let mut cfg = ShowConfig::default();
        let ch = Channel::new("SM");
        let ch_id = ch.id;
        cfg.channels.push(ch);
        let mut a = phrase_auto(
            "x",
            false,
            vec![Action::Delay { seconds: 0.3 }, Action::SendText { channel_id: ch_id, text: "done".into() }],
        );
        a.policy = ExecutionPolicy::IgnoreIfRunning;
        cfg.automations.push(a.clone());
        let hub = Hub::new(cfg, None);
        let eng = Engine::new(hub.clone());
        eng.start();
        tokio::task::yield_now().await;
        hub.add_entry(ch_id, "x".into(), EntrySource::Audio, false);
        hub.add_entry(ch_id, "x".into(), EntrySource::Audio, false);
        tokio::time::sleep(Duration::from_millis(600)).await;
        let done = hub.transcript().iter().filter(|e| e.text == "done").count();
        assert_eq!(done, 1);

        // Cancel-and-restart: two quick triggers give one completion.
        hub.update(|c| c.automations[0].policy = ExecutionPolicy::CancelAndRestart);
        hub.clear_transcript(None);
        hub.add_entry(ch_id, "x".into(), EntrySource::Audio, false);
        tokio::time::sleep(Duration::from_millis(100)).await;
        hub.add_entry(ch_id, "x".into(), EntrySource::Audio, false);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(hub.transcript().iter().filter(|e| e.text == "done").count(), 1);
    }

    #[tokio::test]
    async fn osc_trigger_with_args() {
        let mut cfg = ShowConfig::default();
        let ch = Channel::new("SM");
        let ch_id = ch.id;
        cfg.channels.push(ch);
        cfg.automations.push(Automation {
            id: Uuid::new_v4(),
            name: "eos cue".into(),
            enabled: true,
            policy: ExecutionPolicy::RunParallel,
            cooldown_seconds: 0.0,
            trigger: Trigger::Osc { address: "/eos/out/event/cue/*/*/fire".into() },
            actions: vec![Action::SendText { channel_id: ch_id, text: "Cue fired: {address}".into() }],
        });
        let hub = Hub::new(cfg, None);
        let eng = Engine::new(hub.clone());
        eng.start();
        tokio::task::yield_now().await;
        hub.emit(Event::OscReceived { address: "/eos/out/event/cue/1/5/fire".into(), args: vec![], from: "x".into() });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(hub.transcript()[0].text, "Cue fired: /eos/out/event/cue/1/5/fire");
    }
}
