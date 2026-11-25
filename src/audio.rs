//! Audio sources. Every source delivers 16 kHz mono f32 audio per opencom channel.
//!
//! One sound card stream feeds every opencom channel that uses that card: the capture
//! callback splits interleaved frames by channel index, resamples, and forwards.

use crate::stt::AudioMsg;
use crate::stt::SAMPLE_RATE;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Box-filter + linear-interpolation resampler. Good enough for speech recognition.
pub struct Resampler {
    step: f64, // input samples per output sample
    pos: f64,
    acc: f64,
    acc_n: f64,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self { step: in_rate as f64 / out_rate as f64, pos: 0.0, acc: 0.0, acc_n: 0.0 }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.step - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        if self.step > 1.0 {
            // Downsample: average the input samples that fall in each output window.
            for &s in input {
                self.acc += s as f64;
                self.acc_n += 1.0;
                self.pos += 1.0;
                if self.pos >= self.step {
                    out.push((self.acc / self.acc_n) as f32);
                    self.pos -= self.step;
                    self.acc = 0.0;
                    self.acc_n = 0.0;
                }
            }
        } else {
            // Upsample: repeat with linear steps.
            for &s in input {
                let prev = self.acc as f32;
                while self.pos < 1.0 {
                    out.push(prev + (s - prev) * self.pos as f32);
                    self.pos += self.step;
                }
                self.pos -= 1.0;
                self.acc = s as f64;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDevice {
    /// Stable ID to store in show files, for example `alsa:hw:CARD=B101,DEV=0` or `wasapi:{...}`.
    pub id: String,
    pub name: String,
    pub host: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub is_default: bool,
}

/// A running capture. Dropping it (or calling `stop`) ends the capture thread.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub label: String,
}

impl Capture {
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Play a WAV file into one channel in real time (or as fast as possible when `realtime` is false).
pub fn start_file(path: &str, looped: bool, realtime: bool, tx: Sender<AudioMsg>) -> Result<Capture> {
    let samples = read_wav_mono_16k(path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let label = format!("file:{path}");
    let thread = std::thread::Builder::new().name(label.clone()).spawn(move || {
        let chunk = SAMPLE_RATE as usize / 10;
        loop {
            let start = Instant::now();
            for (i, c) in samples.chunks(chunk).enumerate() {
                if s2.load(Ordering::SeqCst) {
                    return;
                }
                if tx.send(AudioMsg::Samples(c.to_vec())).is_err() {
                    return;
                }
                if realtime {
                    let due = start + Duration::from_millis(100 * (i as u64 + 1));
                    if let Some(wait) = due.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                }
            }
            if !looped {
                // Add trailing silence so the last utterance ends cleanly.
                let _ = tx.send(AudioMsg::Samples(vec![0.0; SAMPLE_RATE as usize]));
                let _ = tx.send(AudioMsg::End);
                return;
            }
        }
    })?;
    Ok(Capture { stop, thread: Some(thread), label })
}

/// Read a WAV file, mix to mono, resample to 16 kHz.
pub fn read_wav_mono_16k(path: &str) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("opening {path}"))?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    let mono: Vec<f32> = interleaved.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    let mut out = Vec::with_capacity(mono.len());
    Resampler::new(spec.sample_rate, SAMPLE_RATE).process(&mono, &mut out);
    Ok(out)
}

#[cfg(feature = "audio")]
pub use device::*;

#[cfg(feature = "audio")]
mod device {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleFormat, StreamConfig};

    fn device_name(d: &cpal::Device) -> String {
        d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| "unknown".into())
    }

    fn device_id(d: &cpal::Device) -> String {
        d.id().map(|i| i.to_string()).unwrap_or_default()
    }

    /// ALSA lists every plugin as a device. Keep the useful ones: the default device,
    /// PipeWire/Pulse, and raw hardware (`hw:`). Set OPENCOM_ALL_DEVICES=1 to see all.
    fn useful(id: &str) -> bool {
        if std::env::var_os("OPENCOM_ALL_DEVICES").is_some() {
            return true;
        }
        match id.strip_prefix("alsa:") {
            Some(pcm) => {
                pcm == "default"
                    || pcm == "pipewire"
                    || pcm == "pulse"
                    // Named hardware only; `hw:CARD=0` duplicates `hw:CARD=PCH`.
                    || (pcm.starts_with("hw:CARD=") && !pcm["hw:CARD=".len()..].starts_with(|c: char| c.is_ascii_digit()))
            }
            None => true,
        }
    }

    /// Run `f` with stderr silenced. ALSA's JACK plugin prints noise when probed.
    #[cfg(target_os = "linux")]
    fn quiet<R>(f: impl FnOnce() -> R) -> R {
        // SAFETY: plain fd juggling on stderr; restored before returning.
        unsafe {
            let saved = libc::dup(2);
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if saved < 0 || null < 0 {
                return f();
            }
            libc::dup2(null, 2);
            libc::close(null);
            let r = f();
            libc::dup2(saved, 2);
            libc::close(saved);
            r
        }
    }
    #[cfg(not(target_os = "linux"))]
    fn quiet<R>(f: impl FnOnce() -> R) -> R {
        f()
    }

    pub fn list_inputs() -> Vec<InputDevice> {
        quiet(|| {
            let mut out = Vec::new();
            for host_id in cpal::available_hosts() {
                let Ok(host) = cpal::host_from_id(host_id) else { continue };
                let default_id = host.default_input_device().map(|d| device_id(&d));
                let Ok(devs) = host.input_devices() else { continue };
                for d in devs {
                    let id = device_id(&d);
                    if id.is_empty() || !useful(&id) || out.iter().any(|x: &InputDevice| x.id == id) {
                        continue;
                    }
                    let max_ch = d.supported_input_configs().map(|c| c.map(|r| r.channels()).max().unwrap_or(0)).unwrap_or(0);
                    if max_ch == 0 {
                        continue;
                    }
                    let rate = d.default_input_config().map(|c| c.sample_rate()).unwrap_or(0);
                    let mut name = device_name(&d);
                    if let Some(pcm) = id.strip_prefix("alsa:") {
                        if !pcm.starts_with("hw:") {
                            name = format!("{name} ({pcm})");
                        }
                    }
                    out.push(InputDevice { is_default: default_id.as_deref() == Some(id.as_str()), id, name, host: host_id.name().to_string(), channels: max_ch, sample_rate: rate });
                }
            }
            out
        })
    }

    /// Find a device by stored ID, then by exact name, then by name substring.
    fn find_device(key: Option<&str>) -> Result<cpal::Device> {
        quiet(|| {
            let host = cpal::default_host();
            let Some(n) = key else {
                return host.default_input_device().ok_or_else(|| anyhow!("no default input device"));
            };
            if let Ok(id) = n.parse::<cpal::DeviceId>() {
                for host_id in cpal::available_hosts() {
                    if let Some(d) = cpal::host_from_id(host_id).ok().and_then(|h| h.device_by_id(&id)) {
                        return Ok(d);
                    }
                }
            }
            let mut all = Vec::new();
            for host_id in cpal::available_hosts() {
                if let Ok(devs) = cpal::host_from_id(host_id).and_then(|h| h.input_devices()) {
                    all.extend(devs);
                }
            }
            if let Some(i) = all.iter().position(|d| device_name(d) == n || device_id(d) == n) {
                return Ok(all.swap_remove(i));
            }
            let want = n.to_lowercase();
            all.into_iter().find(|d| device_name(d).to_lowercase().contains(&want)).ok_or_else(|| anyhow!("input device {n:?} not found"))
        })
    }

    /// Open one device and route the given channel indexes to their senders.
    pub fn start_device(name: Option<&str>, routes: Vec<(u16, Sender<AudioMsg>)>) -> Result<Capture> {
        let dev = find_device(name)?;
        let dname = device_name(&dev);
        let max_ch = dev.supported_input_configs()?.map(|r| r.channels()).max().unwrap_or(1);
        let need = routes.iter().map(|(i, _)| i + 1).max().unwrap_or(1);
        if need > max_ch {
            return Err(anyhow!("device {dname} has {max_ch} input channels; channel {} requested", need));
        }
        let def = dev.default_input_config()?;
        let format = def.sample_format();
        let rate = def.sample_rate();
        // Open with all channels so any index works.
        let channels = if def.channels() >= need { def.channels() } else { max_ch };
        let config = StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default };

        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let label = format!("device:{dname}");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let thread = std::thread::Builder::new().name(label.clone()).spawn(move || {
            let mut sinks: Vec<(usize, Sender<AudioMsg>, Resampler, Vec<f32>, Vec<f32>)> =
                routes.into_iter().map(|(i, tx)| (i as usize, tx, Resampler::new(rate, SAMPLE_RATE), Vec::new(), Vec::new())).collect();
            let nch = channels as usize;
            let mut deliver = move |data: &[f32]| {
                for (idx, tx, rs, mono, out) in sinks.iter_mut() {
                    mono.clear();
                    mono.extend(data.chunks(nch).map(|f| f.get(*idx).copied().unwrap_or(0.0)));
                    rs.process(mono, out);
                    if out.len() >= 1600 {
                        let _ = tx.send(AudioMsg::Samples(std::mem::take(out)));
                    }
                }
            };
            let err = |e: cpal::Error| tracing::warn!("audio stream error: {e}");
            let stream = match format {
                SampleFormat::F32 => dev.build_input_stream::<f32, _, _>(config, move |d: &[f32], _| deliver(d), err, None),
                SampleFormat::I16 => {
                    let mut buf = Vec::new();
                    dev.build_input_stream::<i16, _, _>(
                        config,
                        move |d: &[i16], _| {
                            buf.clear();
                            buf.extend(d.iter().map(|&s| s as f32 / 32768.0));
                            deliver(&buf)
                        },
                        err,
                        None,
                    )
                }
                SampleFormat::I32 => {
                    let mut buf = Vec::new();
                    dev.build_input_stream::<i32, _, _>(
                        config,
                        move |d: &[i32], _| {
                            buf.clear();
                            buf.extend(d.iter().map(|&s| s as f32 / 2147483648.0));
                            deliver(&buf)
                        },
                        err,
                        None,
                    )
                }
                SampleFormat::U16 => {
                    let mut buf = Vec::new();
                    dev.build_input_stream::<u16, _, _>(
                        config,
                        move |d: &[u16], _| {
                            buf.clear();
                            buf.extend(d.iter().map(|&s| (s as f32 - 32768.0) / 32768.0));
                            deliver(&buf)
                        },
                        err,
                        None,
                    )
                }
                other => {
                    let _ = ready_tx.send(Err(anyhow!("unsupported sample format {other:?}")));
                    return;
                }
            };
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(anyhow!("opening input stream: {e}")));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(anyhow!("starting input stream: {e}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            while !s2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(stream);
        })?;
        ready_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("audio device did not start in time"))??;
        Ok(Capture { stop, thread: Some(thread), label })
    }
}

#[cfg(not(feature = "audio"))]
pub fn list_inputs() -> Vec<InputDevice> {
    vec![]
}

#[cfg(not(feature = "audio"))]
pub fn start_device(_name: Option<&str>, _routes: Vec<(u16, Sender<AudioMsg>)>) -> Result<Capture> {
    Err(anyhow!("this build has no live audio input (built without the `audio` feature)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_lengths() {
        let input = vec![0.5f32; 48000];
        let mut out = Vec::new();
        Resampler::new(48000, 16000).process(&input, &mut out);
        assert!((out.len() as i64 - 16000).abs() <= 1);
        assert!((out[100] - 0.5).abs() < 1e-6);
        let mut up = Vec::new();
        Resampler::new(8000, 16000).process(&vec![0.25f32; 8000], &mut up);
        assert!((up.len() as i64 - 16000).abs() <= 2);
        let mut odd = Vec::new();
        Resampler::new(44100, 16000).process(&vec![0.1f32; 44100], &mut odd);
        assert!((odd.len() as i64 - 16000).abs() <= 1);
    }
}
