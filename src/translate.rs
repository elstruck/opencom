//! Optional translation of final transcript lines through an OpenAI-compatible chat API.

use crate::hub::{Event, SharedHub};
use crate::model::{EntrySource, TranslationSettings};
use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use uuid::Uuid;

pub async fn translate(client: &reqwest::Client, cfg: &TranslationSettings, text: &str) -> Result<String> {
    let url = format!("{}/chat/completions", cfg.endpoint.trim_end_matches('/'));
    let system = format!(
        "You translate short radio and intercom messages from a live show into {lang}. \
         Reply with the translation only: no quotes, no notes, no explanations. \
         Keep names, numbers, cue numbers and technical terms. \
         If the message is already in {lang}, reply with it unchanged.",
        lang = cfg.target_language
    );
    let body = json!({
        "model": cfg.model,
        "temperature": 0.1,
        "max_tokens": 400,
        "messages": [ { "role": "system", "content": system }, { "role": "user", "content": text } ],
    });
    let mut req = client.post(url).json(&body);
    if let Some(k) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
        req = req.bearer_auth(k);
    }
    let v: serde_json::Value = req.send().await?.error_for_status()?.json().await?;
    let out = v["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow!("unexpected response from translation API"))?;
    // Drop "thinking" blocks that some local models add.
    let out = match out.rfind("</think>") {
        Some(i) => &out[i + 8..],
        None => out,
    };
    Ok(out.trim().trim_matches('"').trim().to_string())
}

/// Watch final entries and translate the ones from channels with translation on.
pub fn start(hub: SharedHub) {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("http client");
    let limit = Arc::new(Semaphore::new(4));
    let mut rx = hub.subscribe();
    tokio::spawn(async move {
        let mut done: HashSet<Uuid> = HashSet::new();
        let warned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        loop {
            let ev = match rx.recv().await {
                Ok(e) => e,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let entry = match ev {
                Event::EntryAdded { entry } | Event::EntryUpdated { entry } => entry,
                _ => continue,
            };
            if entry.in_progress || entry.translation.is_some() || entry.source == EntrySource::Automation || entry.text.trim().is_empty() {
                continue;
            }
            let cfg = hub.read(|c| c.settings.translation.clone());
            if !cfg.enabled || !hub.channel(entry.channel_id).map(|c| c.translate).unwrap_or(false) {
                continue;
            }
            if !done.insert(entry.id) {
                continue;
            }
            if done.len() > 20_000 {
                done.clear();
            }
            let (hub2, client2, permit) = (hub.clone(), client.clone(), limit.clone().acquire_owned().await);
            let warned = warned.clone();
            tokio::spawn(async move {
                let _p = permit;
                match translate(&client2, &cfg, &entry.text).await {
                    Ok(t) if !t.is_empty() && t != entry.text => {
                        warned.store(false, std::sync::atomic::Ordering::Relaxed);
                        hub2.set_translation(entry.id, t);
                    }
                    Ok(_) => warned.store(false, std::sync::atomic::Ordering::Relaxed),
                    Err(e) => {
                        tracing::warn!("translation failed: {e:#}");
                        if !warned.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            hub2.emit(Event::Status { component: "translate".into(), ok: false, message: format!("{e:#}") });
                        }
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Hub;
    use crate::model::{Channel, ShowConfig};
    use axum::{routing::post, Json, Router};

    #[tokio::test]
    async fn translates_final_entries() {
        // A fake OpenAI-compatible server that upper-cases the user message.
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|Json(b): Json<serde_json::Value>| async move {
                let text = b["messages"][1]["content"].as_str().unwrap_or("").to_uppercase();
                Json(json!({ "choices": [ { "message": { "content": format!("<think>x</think> {text}") } } ] }))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

        let mut cfg = ShowConfig::default();
        let mut ch = Channel::new("ES");
        ch.translate = true;
        let id = ch.id;
        cfg.channels.push(ch);
        cfg.settings.translation = TranslationSettings { enabled: true, endpoint: format!("http://127.0.0.1:{port}/v1"), ..Default::default() };
        let hub = Hub::new(cfg, None);
        start(hub.clone());
        tokio::task::yield_now().await;
        let e = hub.add_entry(id, "hola".into(), EntrySource::Audio, true);
        hub.update_entry(e.id, "hola mundo".into(), false);
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if hub.entry(e.id).and_then(|e| e.translation).is_some() {
                break;
            }
        }
        assert_eq!(hub.entry(e.id).unwrap().translation.as_deref(), Some("HOLA MUNDO"));
    }
}
