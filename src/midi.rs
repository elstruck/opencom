//! MIDI output. Built with the `midi` feature; without it, sends fail with a clear error.

use crate::model::MidiMessage;
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiPort {
    pub name: String,
    pub is_output: bool,
    pub is_input: bool,
}

#[cfg(feature = "midi")]
mod imp {
    use super::*;
    use anyhow::{anyhow, Context};
    use midir::{MidiInput, MidiOutput, MidiOutputConnection};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    fn conns() -> &'static Mutex<HashMap<String, MidiOutputConnection>> {
        static C: OnceLock<Mutex<HashMap<String, MidiOutputConnection>>> = OnceLock::new();
        C.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub fn list_ports() -> Vec<MidiPort> {
        let mut out = Vec::new();
        if let Ok(o) = MidiOutput::new("opencom-list") {
            for p in o.ports() {
                if let Ok(name) = o.port_name(&p) {
                    out.push(MidiPort { name, is_output: true, is_input: false });
                }
            }
        }
        if let Ok(i) = MidiInput::new("opencom-list") {
            for p in i.ports() {
                if let Ok(name) = i.port_name(&p) {
                    if let Some(e) = out.iter_mut().find(|e| e.name == name) {
                        e.is_input = true;
                    } else {
                        out.push(MidiPort { name, is_output: false, is_input: true });
                    }
                }
            }
        }
        out
    }

    pub fn send(port: Option<&str>, msg: &MidiMessage) -> Result<()> {
        let key = port.unwrap_or("").to_string();
        let mut map = conns().lock();
        if !map.contains_key(&key) {
            let out = MidiOutput::new("opencom").context("MIDI output not available")?;
            let ports = out.ports();
            let chosen = ports
                .iter()
                .find(|p| match port {
                    Some(want) => out.port_name(p).map(|n| n.to_lowercase().contains(&want.to_lowercase())).unwrap_or(false),
                    None => true,
                })
                .ok_or_else(|| anyhow!("no MIDI output port matches {:?}", port))?
                .clone();
            let conn = out.connect(&chosen, "opencom-out").map_err(|e| anyhow!("MIDI connect: {e}"))?;
            map.insert(key.clone(), conn);
        }
        let conn = map.get_mut(&key).unwrap();
        if let Err(e) = conn.send(&msg.to_bytes()) {
            map.remove(&key);
            return Err(anyhow!("MIDI send: {e}"));
        }
        Ok(())
    }
}

#[cfg(not(feature = "midi"))]
mod imp {
    use super::*;
    pub fn list_ports() -> Vec<MidiPort> {
        vec![]
    }
    pub fn send(_port: Option<&str>, _msg: &MidiMessage) -> Result<()> {
        anyhow::bail!("this build has no MIDI support (built without the `midi` feature)")
    }
}

pub use imp::{list_ports, send};
