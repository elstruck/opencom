//! Model folder location and downloads.

use super::catalog::{self, ModelSpec, RELEASE_BASE, VAD_FILE};
use crate::model::SttSettings;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn default_models_dir() -> PathBuf {
    directories::ProjectDirs::from("io", "opencom", "opencom")
        .map(|d| d.data_dir().join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}

pub fn models_dir(cfg: &SttSettings) -> PathBuf {
    cfg.models_dir.as_ref().map(PathBuf::from).unwrap_or_else(default_models_dir)
}

pub fn vad_path(cfg: &SttSettings) -> PathBuf {
    models_dir(cfg).join(VAD_FILE)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    #[serde(flatten)]
    pub spec: ModelSpec,
    pub installed: bool,
    pub active: bool,
}

pub fn list(cfg: &SttSettings) -> Vec<ModelStatus> {
    let dir = models_dir(cfg);
    catalog::MODELS
        .iter()
        .map(|m| ModelStatus { spec: m.clone(), installed: is_installed(&dir, m), active: m.id == cfg.model })
        .collect()
}

pub fn is_installed(dir: &Path, m: &ModelSpec) -> bool {
    m.files.iter().all(|(_, f)| dir.join(m.dir).join(f).exists())
}

/// Download a file to `dest`, reporting progress as (done, total) bytes.
async fn download(url: &str, dest: &Path, progress: &(dyn Fn(u64, u64) + Send + Sync)) -> Result<()> {
    let client = reqwest::Client::builder().build()?;
    let mut resp = client.get(url).send().await?.error_for_status().with_context(|| format!("GET {url}"))?;
    let total = resp.content_length().unwrap_or(0);
    let tmp = dest.with_extension("part");
    let mut f = std::fs::File::create(&tmp)?;
    let mut done = 0u64;
    let mut last = 0u64;
    while let Some(chunk) = resp.chunk().await? {
        f.write_all(&chunk)?;
        done += chunk.len() as u64;
        if done - last > 4 << 20 || done == total {
            progress(done, total);
            last = done;
        }
    }
    f.flush()?;
    drop(f);
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Make sure the Silero VAD model exists.
pub async fn ensure_vad(cfg: &SttSettings) -> Result<PathBuf> {
    let p = vad_path(cfg);
    if !p.exists() {
        std::fs::create_dir_all(p.parent().unwrap())?;
        download(&format!("{RELEASE_BASE}/{VAD_FILE}"), &p, &|_, _| {}).await?;
    }
    Ok(p)
}

/// Download and unpack a catalog model.
pub async fn install(cfg: &SttSettings, id: &str, progress: &(dyn Fn(u64, u64) + Send + Sync)) -> Result<PathBuf> {
    let spec = catalog::find(id).ok_or_else(|| anyhow!("unknown model {id:?}"))?;
    let dir = models_dir(cfg);
    std::fs::create_dir_all(&dir)?;
    ensure_vad(cfg).await?;
    if is_installed(&dir, spec) {
        return Ok(dir.join(spec.dir));
    }
    let archive = dir.join(format!("{}.tar.bz2", spec.dir));
    download(&spec.archive_url(), &archive, progress).await?;
    let d2 = dir.clone();
    let a2 = archive.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let f = std::fs::File::open(&a2)?;
        let dec = bzip2::read::BzDecoder::new(std::io::BufReader::new(f));
        tar::Archive::new(dec).unpack(&d2).context("unpacking model archive")?;
        Ok(())
    })
    .await??;
    let _ = std::fs::remove_file(&archive);
    if !is_installed(&dir, spec) {
        return Err(anyhow!("archive unpacked but model files are missing in {}", dir.join(spec.dir).display()));
    }
    Ok(dir.join(spec.dir))
}
