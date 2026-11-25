//! OSC over UDP: sending, receiving, and address pattern matching.

use crate::hub::{Event, SharedHub};
use crate::model::{OscArg, OscArgKind};
use anyhow::{anyhow, Context, Result};
use rosc::{OscMessage, OscPacket, OscType};
use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub fn build_args(args: &[OscArg], vars: &crate::template::Vars) -> Result<Vec<OscType>> {
    args.iter()
        .map(|a| {
            let v = vars.expand(&a.value);
            Ok(match a.kind {
                OscArgKind::Int => OscType::Int(v.trim().parse().with_context(|| format!("OSC int argument {v:?}"))?),
                OscArgKind::Float => OscType::Float(v.trim().parse().with_context(|| format!("OSC float argument {v:?}"))?),
                OscArgKind::String => OscType::String(v),
                OscArgKind::Bool => OscType::Bool(matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")),
            })
        })
        .collect()
}

pub async fn send(host: &str, port: u16, address: &str, args: Vec<OscType>) -> Result<()> {
    if !address.starts_with('/') {
        return Err(anyhow!("OSC address must start with '/': {address}"));
    }
    let packet = OscPacket::Message(OscMessage { addr: address.to_string(), args });
    let buf = rosc::encoder::encode(&packet).map_err(|e| anyhow!("OSC encode: {e:?}"))?;
    let target: SocketAddr = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("resolving {host}"))?
        .next()
        .ok_or_else(|| anyhow!("no address for {host}"))?;
    let bind = if target.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let sock = UdpSocket::bind(bind).await?;
    sock.send_to(&buf, target).await?;
    Ok(())
}

pub fn arg_to_string(a: &OscType) -> String {
    match a {
        OscType::Int(i) => i.to_string(),
        OscType::Long(i) => i.to_string(),
        OscType::Float(f) => f.to_string(),
        OscType::Double(f) => f.to_string(),
        OscType::String(s) => s.clone(),
        OscType::Bool(b) => b.to_string(),
        OscType::Nil => "nil".into(),
        OscType::Inf => "inf".into(),
        other => format!("{other:?}"),
    }
}

/// OSC-style address match. `*` matches within one segment, `?` one char,
/// `**` matches any number of segments, `{a,b}` alternatives inside a segment.
pub fn address_matches(pattern: &str, address: &str) -> bool {
    let p: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let a: Vec<&str> = address.trim_matches('/').split('/').collect();
    match_segments(&p, &a)
}

fn match_segments(p: &[&str], a: &[&str]) -> bool {
    match (p.first(), a.first()) {
        (None, None) => true,
        (Some(&"**"), _) => (0..=a.len()).any(|i| match_segments(&p[1..], &a[i..])),
        (Some(ps), Some(as_)) => segment_matches(ps, as_) && match_segments(&p[1..], &a[1..]),
        _ => false,
    }
}

fn segment_matches(p: &str, s: &str) -> bool {
    // Expand {a,b} alternatives first.
    if let (Some(open), Some(close)) = (p.find('{'), p.find('}')) {
        if open < close {
            let (pre, rest) = p.split_at(open);
            let alts = &rest[1..close - open];
            let post = &rest[close - open + 1..];
            return alts.split(',').any(|alt| segment_matches(&format!("{pre}{alt}{post}"), s));
        }
    }
    glob(p.as_bytes(), s.as_bytes())
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob(&p[1..], s) || (!s.is_empty() && glob(p, &s[1..])),
        (Some(b'?'), Some(_)) => glob(&p[1..], &s[1..]),
        (Some(pc), Some(sc)) if pc == sc => glob(&p[1..], &s[1..]),
        _ => false,
    }
}

fn flatten(packet: OscPacket, out: &mut Vec<OscMessage>) {
    match packet {
        OscPacket::Message(m) => out.push(m),
        OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
    }
}

/// Listen for OSC on `port` and publish every message as `Event::OscReceived`.
/// Also accepts a small control namespace:
///   /opencom/send <channel> <text>   post text into a channel
///   /opencom/alert <text>            flash an alert
///   /opencom/automation <name|id>    run an automation
pub async fn listen(hub: SharedHub, port: u16) -> Result<()> {
    let sock = UdpSocket::bind(("0.0.0.0", port)).await.with_context(|| format!("binding OSC port {port}"))?;
    tracing::info!("OSC listening on udp/{port}");
    hub.emit(Event::Status { component: "osc".into(), ok: true, message: format!("listening on udp/{port}") });
    let mut buf = vec![0u8; 65536];
    loop {
        let (n, from) = sock.recv_from(&mut buf).await?;
        let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..n]) else {
            tracing::debug!("bad OSC packet from {from}");
            continue;
        };
        let mut msgs = Vec::new();
        flatten(packet, &mut msgs);
        for m in msgs {
            let args: Vec<String> = m.args.iter().map(arg_to_string).collect();
            handle_control(&hub, &m.addr, &args);
            hub.emit(Event::OscReceived { address: m.addr, args, from: from.to_string() });
        }
    }
}

fn handle_control(hub: &SharedHub, addr: &str, args: &[String]) {
    match addr {
        "/opencom/send" if args.len() >= 2 => {
            if let Some(ch) = hub.resolve_channel(&args[0]) {
                hub.add_entry(ch.id, args[1..].join(" "), crate::model::EntrySource::Typed, false);
            }
        }
        "/opencom/alert" if !args.is_empty() => {
            hub.emit(Event::Alert { text: args.join(" "), seconds: 3.0, color: None });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(address_matches("/eos/out/event/cue/*/*/fire", "/eos/out/event/cue/1/12/fire"));
        assert!(!address_matches("/eos/out/event/cue/*/fire", "/eos/out/event/cue/1/12/fire"));
        assert!(address_matches("/eos/**", "/eos/out/event/cue/1/12/fire"));
        assert!(address_matches("/cue/{go,stop}", "/cue/stop"));
        assert!(address_matches("/ch/?", "/ch/5"));
        assert!(!address_matches("/ch/?", "/ch/12"));
        assert!(address_matches("/a/**/z", "/a/z"));
    }

    #[tokio::test]
    async fn send_and_receive() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = rx.local_addr().unwrap().port();
        send("127.0.0.1", port, "/test/go", vec![OscType::Int(7), OscType::String("hi".into())]).await.unwrap();
        let mut buf = [0u8; 1024];
        let (n, _) = rx.recv_from(&mut buf).await.unwrap();
        let (_, p) = rosc::decoder::decode_udp(&buf[..n]).unwrap();
        let OscPacket::Message(m) = p else { panic!() };
        assert_eq!(m.addr, "/test/go");
        assert_eq!(m.args, vec![OscType::Int(7), OscType::String("hi".into())]);
    }
}
