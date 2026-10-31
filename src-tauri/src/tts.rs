//! TTS 语音输出：把桌宠说的话读出来。默认不读，用户在设置里选了引擎才读。
//!
//! 两种引擎：
//! - 云端（`cloud`）：OpenAI 兼容的 `/audio/speech`（OpenAI、硅基流动等），在「模型」里配一个服务商、选为"语音合成"用途；
//! - 本地服务（`local`）：GPT-SoVITS 风格的 HTTP 接口（`refer_wav_path` + `text`），用户自己跑服务、自己提供参考音频。
//!
//! 都要 wav 格式的音频，解码后用默认的扬声器播放。读什么：
//! - 桌宠主动说的话（agent 汇报、主动聊天）整句读；
//! - 对话回复只读开头一小段（`reply_chars`），其余看文字。
//!
//! 什么时候不出声：桌宠隐藏、全屏 / 开会 / 直播（静默状态）时不读；工作模式和免打扰时段默认也不读。
//! 开始录音、发新消息、读下一句时打断正在读的。

use crate::config::{Config, TtsParams};
use crate::llm::Endpoint;
use crate::AppState;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// 读的是什么（设置里可以分开开关）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    Reply,
    Agent,
    System,
    Proactive,
}

impl From<crate::speak::Source> for What {
    fn from(s: crate::speak::Source) -> Self {
        match s {
            crate::speak::Source::Agent => What::Agent,
            crate::speak::Source::System => What::System,
            crate::speak::Source::Proactive => What::Proactive,
        }
    }
}

/// 这类内容要不要读（只看设置，不看当前能不能出声）
pub fn wants(p: &TtsParams, what: What) -> bool {
    p.enabled
        && p.provider != "none"
        && match what {
            What::Reply => p.read_replies,
            What::Agent | What::Proactive => p.read_reports,
            What::System => false,
        }
}

/// 现在能不能出声：桌宠隐藏、静默状态（全屏、开会、直播）不出声；工作模式、免打扰时段按设置
fn sound_allowed(app: &AppHandle, p: &TtsParams) -> bool {
    let state = app.state::<AppState>();
    if state.shared.lock().unwrap().pet_hidden {
        return false;
    }
    let (silent, quiet) = crate::proactive::sound_state(app);
    !silent && (!quiet || !p.mute_when_quiet)
}

/// 每次开始读或打断时加一；播放中发现它变了就停
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// 按设置读一段文字（不读就什么都不做）。在后台读，不阻塞调用方
pub fn speak(app: &AppHandle, text: &str, what: What) {
    let cfg = app.state::<AppState>().config.lock().unwrap().clone();
    let p = &cfg.tts;
    if !wants(p, what) || !sound_allowed(app, p) {
        return;
    }
    let mut text = plain_text(text);
    if what == What::Reply {
        text = excerpt(&text, p.reply_chars.max(10));
    }
    if text.is_empty() {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = synth_and_play(&app, &cfg, &text, generation).await {
            eprintln!("[tts] 朗读失败：{e}");
        }
    });
}

/// 设置页的"试听"：不管开关和当前状态，用传进来的设置读一句，出错返回原因
pub async fn test(app: &AppHandle, cfg: Config, text: &str) -> Result<(), String> {
    let text = plain_text(text);
    if text.is_empty() {
        return Err("写一句要试听的话".into());
    }
    if cfg.tts.provider == "none" {
        return Err("先选一个朗读引擎".into());
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    synth_and_play(app, &cfg, &text, generation).await
}

/// 打断正在读的（开始录音、发新消息时）
pub fn stop() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

async fn synth_and_play(app: &AppHandle, cfg: &Config, text: &str, generation: u64) -> Result<(), String> {
    let wav = synthesize(cfg, text).await?;
    if GENERATION.load(Ordering::SeqCst) != generation {
        return Ok(()); // 合成期间被打断或者有新的要读
    }
    let (samples, rate) = decode_wav(&wav)?;
    if samples.is_empty() {
        return Ok(());
    }
    // 读的时候桌宠播放说话动画
    let secs = samples.len() as f64 / rate as f64;
    crate::chat::set_speech(app, Some("talk"), Some(Instant::now() + Duration::from_secs_f64(secs + 0.2)));
    let volume = cfg.tts.volume.clamp(0.0, 2.0) as f32;
    tauri::async_runtime::spawn_blocking(move || play(&samples, rate, volume, generation))
        .await
        .map_err(|e| e.to_string())?
}

// ---------- 引擎 ----------

async fn synthesize(cfg: &Config, text: &str) -> Result<Vec<u8>, String> {
    match cfg.tts.provider.as_str() {
        "cloud" => cloud(cfg, text).await,
        "local" => local(&cfg.tts, text).await,
        other => Err(format!("不认识的朗读引擎：{other}")),
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().unwrap_or_default()
}

/// 读响应：失败时把服务返回的错误说明带上
async fn wav_body(resp: reqwest::Response) -> Result<Vec<u8>, String> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| format!("读取音频失败：{e}"))?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        return Err(format!("HTTP {}：{}", status.as_u16(), body.chars().take(240).collect::<String>()));
    }
    Ok(bytes.to_vec())
}

/// OpenAI 兼容的 /audio/speech
async fn cloud(cfg: &Config, text: &str) -> Result<Vec<u8>, String> {
    let provider = cfg.provider(cfg.roles.tts.as_deref()).ok_or("还没有选语音合成用的模型（设置 → 模型 → 模型用途）")?;
    let ep = Endpoint::from_provider(provider);
    let mut body = serde_json::json!({ "model": ep.model, "input": text, "response_format": "wav" });
    if let Some(v) = cfg.tts.voice.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        body["voice"] = v.into();
    }
    if (cfg.tts.speed - 1.0).abs() > 0.01 {
        body["speed"] = cfg.tts.speed.clamp(0.5, 2.0).into();
    }
    let mut req = http().post(format!("{}/audio/speech", ep.base_url)).timeout(Duration::from_secs(60)).json(&body);
    if let Some(k) = &ep.api_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.map_err(|e| format!("连不上 {}：{e}", ep.base_url))?;
    wav_body(resp).await
}

/// GPT-SoVITS 风格的本地服务：POST JSON，返回 wav
async fn local(p: &TtsParams, text: &str) -> Result<Vec<u8>, String> {
    let url = p.local_url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("本地服务地址要以 http:// 开头，比如 http://127.0.0.1:9880/".into());
    }
    let refer = p.local_ref_audio.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or("还没有选参考音频")?;
    let mut body = serde_json::json!({
        "refer_wav_path": refer,
        "text": text,
        "text_language": p.local_text_lang.trim(),
        "media_type": "wav",
    });
    if let Some(t) = p.local_ref_text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["prompt_text"] = t.into();
        body["prompt_language"] = p.local_ref_lang.trim().into();
    }
    let speed = (p.speed - 1.0).abs() > 0.01;
    if speed {
        body["speed_factor"] = p.speed.clamp(0.5, 2.0).into();
    }
    let send = |body: &serde_json::Value| http().post(url).timeout(Duration::from_secs(120)).json(body).send();
    let resp = send(&body).await.map_err(|e| format!("连不上本地 TTS 服务 {url}：{e}"))?;
    // 有的服务不认 speed_factor：去掉再试一次
    if !resp.status().is_success() && speed {
        body.as_object_mut().unwrap().remove("speed_factor");
        let resp = send(&body).await.map_err(|e| format!("连不上本地 TTS 服务 {url}：{e}"))?;
        return wav_body(resp).await;
    }
    wav_body(resp).await
}

// ---------- 解码和播放 ----------

/// 解析 wav：支持 16 位整数、24 位、32 位整数和 32 位浮点 PCM，多声道混成单声道。
/// 流式服务返回的 wav 头里长度可能是 0 或 0xFFFFFFFF，这时把剩下的数据都当作音频
pub fn decode_wav(b: &[u8]) -> Result<(Vec<f32>, u32), String> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("服务返回的不是 wav 音频".into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32_at(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            let mut format = u16_at(body);
            if format == 0xFFFE && body + 26 <= b.len() {
                format = u16_at(body + 24); // WAVE_FORMAT_EXTENSIBLE：真正的格式在子格式里
            }
            fmt = Some((format, u16_at(body + 2), u32_at(body + 4), u16_at(body + 14)));
        } else if id == b"data" {
            let (format, channels, rate, bits) = fmt.ok_or("wav 里缺少格式信息")?;
            let end = if size == 0 || size == u32::MAX as usize { b.len() } else { (body + size).min(b.len()) };
            let data = &b[body..end];
            let channels = channels.max(1) as usize;
            let bytes = (bits / 8) as usize;
            let sample = |s: &[u8]| -> Option<f32> {
                Some(match (format, bits) {
                    (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
                    (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
                    (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                    (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    _ => return None,
                })
            };
            if bytes == 0 || sample(&vec![0u8; bytes]).is_none() {
                return Err(format!("不支持的 wav 格式（格式 {format}，{bits} 位）"));
            }
            let frame = bytes * channels;
            let out = data
                .chunks_exact(frame)
                .map(|f| f.chunks_exact(bytes).filter_map(sample).sum::<f32>() / channels as f32)
                .collect();
            return Ok((out, rate.max(1)));
        }
        // 块按偶数字节对齐
        pos = body + size + (size & 1);
    }
    Err("wav 里没有音频数据".into())
}

/// 线性插值重采样到 to 赫兹（说话声用这个就够了）
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let n = (input.len() as f64 / ratio).floor() as usize;
    (0..n)
        .map(|i| {
            let x = i as f64 * ratio;
            let j = x.floor() as usize;
            let t = (x - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * t
        })
        .collect()
}

/// 用默认扬声器播放，读完或被打断（GENERATION 变了）才返回
fn play(samples: &[f32], rate: u32, volume: f32, generation: u64) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    let host = cpal::default_host();
    let dev = host.default_output_device().ok_or("没有找到扬声器")?;
    let supported = dev.default_output_config().map_err(|e| format!("读取扬声器格式失败：{e}"))?;
    let out_rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let data: Arc<Vec<f32>> = Arc::new(resample(samples, rate, out_rate).into_iter().map(|s| (s * volume).clamp(-1.0, 1.0)).collect());
    let pos = Arc::new(AtomicUsize::new(0));
    let config = supported.config();
    let err_fn = |e| eprintln!("[tts] 播放出错：{e}");

    macro_rules! build {
        ($t:ty, $conv:expr) => {{
            let (d, p) = (data.clone(), pos.clone());
            dev.build_output_stream::<$t, _, _>(
                config.clone(),
                move |out: &mut [$t], _| {
                    let mut i = p.load(Ordering::Relaxed);
                    for frame in out.chunks_mut(channels.max(1)) {
                        let v = d.get(i).copied().unwrap_or(0.0);
                        i += 1;
                        for s in frame.iter_mut() {
                            *s = $conv(v);
                        }
                    }
                    p.store(i, Ordering::Relaxed);
                },
                err_fn,
                None,
            )
        }};
    }
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build!(f32, |v: f32| v),
        SampleFormat::I16 => build!(i16, |v: f32| (v * 32767.0) as i16),
        SampleFormat::U16 => build!(u16, |v: f32| ((v * 32767.0) + 32768.0) as u16),
        SampleFormat::I32 => build!(i32, |v: f32| (v as f64 * 2_147_483_647.0) as i32),
        other => return Err(format!("不支持的扬声器采样格式 {other:?}")),
    }
    .map_err(|e| format!("打开扬声器失败：{e}"))?;
    stream.play().map_err(|e| format!("开始播放失败：{e}"))?;
    // 等读完（多留一点时间让缓冲区放完），或者被打断
    let total = data.len();
    let deadline = Instant::now() + Duration::from_secs_f64(total as f64 / out_rate as f64 + 2.0);
    while pos.load(Ordering::Relaxed) < total + out_rate as usize / 10 && Instant::now() < deadline {
        if GENERATION.load(Ordering::SeqCst) != generation {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    drop(stream);
    Ok(())
}

// ---------- 文字处理 ----------

/// 对话回复只读开头：按句子取，取到超过 max 字为止；第一句就太长时在 max 字处截断
pub fn excerpt(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    let mut count = 0;
    let mut sentence = String::new();
    for c in text.chars() {
        sentence.push(c);
        if "。！？!?；;…\n".contains(c) || (c == '.' && sentence.chars().count() > 1) {
            let n = sentence.chars().count();
            if count > 0 && count + n > max {
                return out;
            }
            if count == 0 && n > max {
                return sentence.chars().take(max).collect();
            }
            out.push_str(&sentence);
            count += n;
            sentence.clear();
            if count >= max {
                return out;
            }
        }
    }
    if count == 0 {
        return sentence.chars().take(max).collect();
    }
    if count + sentence.chars().count() <= max {
        out.push_str(&sentence);
    }
    out
}

/// 读之前去掉 Markdown 记号：代码块换成"（代码略）"，链接只留文字，去掉强调、标题、引用符号
pub fn plain_text(s: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") {
            if !in_code {
                out.push_str("（代码略）\n");
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let line = line.trim_start().trim_start_matches(['#', '>']).trim_start();
        let line = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).unwrap_or(line);
        // [文字](链接) → 文字
        let mut rest = line;
        let mut buf = String::new();
        while let Some(i) = rest.find('[') {
            match (rest[i..].find("]("), rest[i..].find(')')) {
                (Some(j), Some(k)) if j < k => {
                    buf.push_str(&rest[..i]);
                    buf.push_str(&rest[i + 1..i + j]);
                    rest = &rest[i + k + 1..];
                }
                _ => break,
            }
        }
        buf.push_str(rest);
        out.push_str(&buf.replace(['*', '`', '_'], ""));
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markdown_for_reading() {
        let md = "# 标题\n**好的**，看 [文档](https://x.y/z)：\n```rust\nfn main() {}\n```\n- 第一条\n> 引用";
        assert_eq!(plain_text(md), "标题\n好的，看 文档：\n（代码略）\n第一条\n引用");
        assert_eq!(plain_text("   "), "");
    }

    #[test]
    fn reading_is_off_by_default_and_follows_switches() {
        let mut p = TtsParams::default();
        assert!(!wants(&p, What::Reply) && !wants(&p, What::Agent));
        p.enabled = true;
        assert!(!wants(&p, What::Reply)); // 没选引擎也不读
        p.provider = "cloud".into();
        assert!(wants(&p, What::Reply) && wants(&p, What::Agent) && wants(&p, What::Proactive));
        assert!(!wants(&p, What::System));
        p.read_replies = false;
        assert!(!wants(&p, What::Reply));
    }

    #[test]
    fn excerpt_reads_only_the_beginning() {
        assert_eq!(excerpt("早上好！今天天气不错。要不要出去走走？", 12), "早上好！今天天气不错。");
        assert_eq!(excerpt("短句。", 80), "短句。");
        assert_eq!(excerpt("没有标点的一整段很长很长的话", 5), "没有标点的");
        assert_eq!(excerpt("第一句特别特别长超过上限了。第二句。", 5), "第一句特别");
        assert_eq!(excerpt("好的。剩下的没有句号", 80), "好的。剩下的没有句号");
        assert_eq!(excerpt("Hello there. How are you?", 14), "Hello there.");
    }

    fn wav(format: u16, channels: u16, rate: u32, bits: u16, data: &[u8], data_len: u32) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVE".to_vec();
        v.extend(b"fmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(format.to_le_bytes());
        v.extend(channels.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * channels as u32 * bits as u32 / 8).to_le_bytes());
        v.extend((channels * bits / 8).to_le_bytes());
        v.extend(bits.to_le_bytes());
        v.extend(b"LIST");
        v.extend(3u32.to_le_bytes());
        v.extend([1, 2, 3, 0]); // 奇数长度的块，后面补一个字节
        v.extend(b"data");
        v.extend(data_len.to_le_bytes());
        v.extend(data);
        v
    }

    #[test]
    fn decodes_wav_variants() {
        // 16 位单声道
        let pcm: Vec<u8> = [0i16, 16384, -32768].iter().flat_map(|s| s.to_le_bytes()).collect();
        let (s, r) = decode_wav(&wav(1, 1, 24000, 16, &pcm, pcm.len() as u32)).unwrap();
        assert_eq!((s, r), (vec![0.0, 0.5, -1.0], 24000));
        // 立体声混成单声道
        let st: Vec<u8> = [16384i16, 0, -16384, -16384].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(decode_wav(&wav(1, 2, 48000, 16, &st, st.len() as u32)).unwrap().0, vec![0.25, -0.5]);
        // 32 位浮点；流式服务的长度写成 0xFFFFFFFF
        let f: Vec<u8> = [0.25f32, -0.75].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(decode_wav(&wav(3, 1, 32000, 32, &f, u32::MAX)).unwrap().0, vec![0.25, -0.75]);
        // 不是 wav、不支持的格式
        assert!(decode_wav(b"ID3\x04mp3 data here").unwrap_err().contains("不是 wav"));
        assert!(decode_wav(&wav(1, 1, 8000, 8, &[1, 2], 2)).unwrap_err().contains("不支持"));
    }

    #[test]
    fn resamples_linearly() {
        assert_eq!(resample(&[0.0, 1.0], 1, 1), vec![0.0, 1.0]);
        let up = resample(&[0.0, 1.0, 0.0, -1.0], 1, 2);
        assert_eq!(up, vec![0.0, 0.5, 1.0, 0.5, 0.0, -0.5, -1.0, -1.0]);
        assert_eq!(resample(&[0.0, 1.0, 2.0, 3.0], 2, 1), vec![0.0, 2.0]);
    }
}
