use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opencom::hub::Hub;
use opencom::model::{EntrySource, ShowConfig};
use opencom::stt::{self, catalog, models};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "opencom", version, about = "Live multi-channel transcription and voice-triggered show control")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server and web UI (default).
    Serve {
        /// Show file. Default: the platform config folder.
        #[arg(long)]
        show: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 7420)]
        port: u16,
        /// Open the UI in the default browser.
        #[arg(long)]
        open: bool,
    },
    /// List audio input devices.
    Devices,
    /// List MIDI ports.
    Midi,
    /// List or download speech models.
    Models {
        #[command(subcommand)]
        cmd: Option<ModelsCmd>,
    },
    /// Transcribe a WAV file and print the utterances (for testing models).
    Transcribe {
        file: PathBuf,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 2)]
        threads: i32,
    },
}

#[derive(Subcommand)]
enum ModelsCmd {
    List,
    Download { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "opencom=info,warn".into()))
        .init();
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Serve { show: None, host: "127.0.0.1".into(), port: 7420, open: false }) {
        Cmd::Serve { show, host, port, open } => serve(show.unwrap_or_else(opencom::server::default_show_path), &host, port, open).await,
        Cmd::Devices => {
            let devs = tokio::task::spawn_blocking(opencom::audio::list_inputs).await?;
            if devs.is_empty() {
                println!("No audio input devices found.");
            }
            for d in devs {
                println!("{}{}  [{} ch @ {} Hz]\n    id: {}", if d.is_default { "* " } else { "  " }, d.name, d.channels, d.sample_rate, d.id);
            }
            Ok(())
        }
        Cmd::Midi => {
            for p in opencom::midi::list_ports() {
                println!("{}  {}{}", p.name, if p.is_output { "out " } else { "" }, if p.is_input { "in" } else { "" });
            }
            Ok(())
        }
        Cmd::Models { cmd } => {
            let stt_cfg = opencom::model::SttSettings::default();
            match cmd.unwrap_or(ModelsCmd::List) {
                ModelsCmd::List => {
                    println!("Models folder: {}", models::models_dir(&stt_cfg).display());
                    for m in models::list(&stt_cfg) {
                        println!("{} {:<28} {:>5} MB  {}", if m.installed { "[x]" } else { "[ ]" }, m.spec.id, m.spec.download_mb, m.spec.name);
                    }
                    Ok(())
                }
                ModelsCmd::Download { id } => {
                    let p = models::install(&stt_cfg, &id, &|d, t| {
                        if t > 0 {
                            eprint!("\r{:>3}% ({} / {} MB)", d * 100 / t, d >> 20, t >> 20);
                        }
                    })
                    .await?;
                    eprintln!("\nInstalled in {}", p.display());
                    Ok(())
                }
            }
        }
        Cmd::Transcribe { file, model, threads } => transcribe(file, model, threads).await,
    }
}

async fn transcribe(file: PathBuf, model: Option<String>, threads: i32) -> Result<()> {
    let mut sttc = opencom::model::SttSettings { threads, partial_interval: 0.0, ..Default::default() };
    if let Some(m) = model {
        sttc.model = m;
    }
    catalog::find(&sttc.model).with_context(|| format!("unknown model {}", sttc.model))?;
    let vad = models::ensure_vad(&sttc).await.ok();
    let t0 = Instant::now();
    let rec = stt::load_recognizer(&sttc)?;
    eprintln!("Loaded {} in {:.2}s", rec.name(), t0.elapsed().as_secs_f32());
    let audio = opencom::audio::read_wav_mono_16k(&file.to_string_lossy())?;
    let dur = audio.len() as f32 / stt::SAMPLE_RATE as f32;

    let mut cfg = ShowConfig::default();
    let ch = opencom::model::Channel::new("file");
    let ch_id = ch.id;
    cfg.channels.push(ch);
    let hub = Hub::new(cfg, None);
    let seg = stt::segmenter_factory(vad, &sttc)();
    let mut p = stt::ChannelPipeline::new(ch_id, hub.clone(), rec, seg, stt::PipelineConfig { partial_interval: 0.0, ..Default::default() });
    let t1 = Instant::now();
    for c in audio.chunks(1600) {
        p.feed(c);
    }
    p.feed(&vec![0.0; stt::SAMPLE_RATE as usize]);
    p.finish();
    let took = t1.elapsed().as_secs_f32();
    for e in hub.transcript().iter().filter(|e| e.source == EntrySource::Audio) {
        println!("{}", e.text);
    }
    eprintln!("Audio {dur:.1}s, processed in {took:.2}s (real-time factor {:.3})", took / dur.max(0.001));
    Ok(())
}

async fn serve(show: PathBuf, host: &str, port: u16, open: bool) -> Result<()> {
    let server = opencom::server::start(show, host, port).await?;
    let url = server.url();
    println!("opencom {} running at {url}", env!("CARGO_PKG_VERSION"));
    if open {
        let _ = open_browser(&url);
    }
    tokio::signal::ctrl_c().await?;
    server.stop().await
}

fn open_browser(url: &str) -> std::io::Result<std::process::Child> {
    #[cfg(target_os = "windows")]
    return std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    return std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    return std::process::Command::new("xdg-open").arg(url).spawn();
}
