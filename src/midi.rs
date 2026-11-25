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

/// Parse a channel voice message into (kind, channel, number, value).
pub fn parse(bytes: &[u8]) -> Option<(&'static str, u8, u8, u8)> {
    let status = *bytes.first()?;
    let ch = status & 0x0F;
    match status & 0xF0 {
        0x90 if bytes.get(2).copied().unwrap_or(0) > 0 => Some(("noteOn", ch, *bytes.get(1)?, *bytes.get(2)?)),
        0x90 | 0x80 => Some(("noteOff", ch, *bytes.get(1)?, bytes.get(2).copied().unwrap_or(0))),
        0xB0 => Some(("controlChange", ch, *bytes.get(1)?, *bytes.get(2)?)),
        0xC0 => Some(("programChange", ch, *bytes.get(1)?, 0)),
        _ => None,
    }
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

    /// Listen on every MIDI input (except our own output) and publish `MidiReceived` events.
    /// Keep the returned value alive; dropping it closes the inputs.
    pub fn start_inputs(hub: crate::hub::SharedHub) -> Vec<midir::MidiInputConnection<()>> {
        let mut conns = Vec::new();
        let Ok(probe) = MidiInput::new("opencom-probe") else { return conns };
        for p in probe.ports() {
            let Ok(name) = probe.port_name(&p) else { continue };
            if name.contains("opencom") {
                continue;
            }
            let Ok(mut input) = MidiInput::new("opencom-in") else { continue };
            input.ignore(midir::Ignore::All);
            let h = hub.clone();
            let n2 = name.clone();
            match input.connect(
                &p,
                "opencom-in",
                move |_ts, bytes, _| {
                    if let Some((kind, channel, number, value)) = super::parse(bytes) {
                        h.emit(crate::hub::Event::MidiReceived { port: n2.clone(), kind: kind.into(), channel, number, value });
                    }
                },
                (),
            ) {
                Ok(c) => {
                    tracing::info!("MIDI input: {name}");
                    conns.push(c);
                }
                Err(e) => tracing::warn!("MIDI input {name}: {e}"),
            }
        }
        conns
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
    pub fn start_inputs(_hub: crate::hub::SharedHub) -> Vec<()> {
        vec![]
    }
    pub fn list_ports() -> Vec<MidiPort> {
        vec![]
    }
    pub fn send(_port: Option<&str>, _msg: &MidiMessage) -> Result<()> {
        anyhow::bail!("this build has no MIDI support (built without the `midi` feature)")
    }
}

pub use imp::{list_ports, send, start_inputs};

#[cfg(test)]
mod tests {
    #[test]
    fn parse_messages() {
        assert_eq!(super::parse(&[0x91, 60, 100]), Some(("noteOn", 1, 60, 100)));
        assert_eq!(super::parse(&[0x90, 60, 0]), Some(("noteOff", 0, 60, 0)));
        assert_eq!(super::parse(&[0xB2, 7, 127]), Some(("controlChange", 2, 7, 127)));
        assert_eq!(super::parse(&[0xC0, 5]), Some(("programChange", 0, 5, 0)));
        assert_eq!(super::parse(&[0xF8]), None);
    }
}
