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
//! 选了朗读语言时，朗读前先用辅助模型翻译成这种语言，显示的文字不变；翻译失败就读原文。
//!
//! 什么时候不出声：桌宠隐藏、全屏 / 开会 / 直播（静默状态）时不读；工作模式和免打扰时段默认也不读。
//! 开始录音、发新消息、读下一句时打断正在读的。

use crate::config::{Config, TtsParams};
use crate::llm::{self, ChatMessage, Endpoint};
use crate::AppState;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

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

/// 正在处理的朗读数（翻译、合成、播放都算）。新的一句打断旧的时会短暂有两个
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// 告诉气泡有没有在读：读完之前别收起。从第一句开始、到最后一句结束各发一次
fn set_active(app: &AppHandle, on: bool) {
    let changed = if on { ACTIVE.fetch_add(1, Ordering::SeqCst) == 0 } else { ACTIVE.fetch_sub(1, Ordering::SeqCst) == 1 };
    if changed {
        let _ = app.emit_to(crate::windows::BUBBLE, "tts-speaking", on);
    }
}

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
    set_active(&app, true);
    tauri::async_runtime::spawn(async move {
        if let Err(e) = synth_and_play(&app, &cfg, &text, generation, false).await {
            eprintln!("[tts] 朗读失败：{e}");
        }
        set_active(&app, false);
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
    // 试听时翻译失败要让用户看到原因
    synth_and_play(app, &cfg, &text, generation, true).await
}

/// 打断正在读的（开始录音、发新消息时）
pub fn stop() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

async fn synth_and_play(app: &AppHandle, cfg: &Config, text: &str, generation: u64, strict: bool) -> Result<(), String> {
    let translated = match translate(cfg, text).await {
        Ok(t) => t,
        Err(e) if strict => return Err(e),
        Err(e) => {
            eprintln!("[tts] 翻译失败，读原文：{e}");
            None
        }
    };
    if GENERATION.load(Ordering::SeqCst) != generation {
        return Ok(()); // 翻译期间被打断或者有新的要读
    }
    let lang = translated.as_ref().and(speak_language(cfg));
    let wav = synthesize(cfg, translated.as_deref().unwrap_or(text), lang).await?;
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

// ---------- 朗读语言 ----------

fn speak_language(cfg: &Config) -> Option<&str> {
    filled(&cfg.tts.speak_language)
}

/// 写给翻译模型看的语言名
fn language_name(code: &str) -> &str {
    match code {
        "zh" => "中文",
        "en" => "英语",
        "ja" => "日语",
        "ko" => "韩语",
        "de" => "德语",
        "fr" => "法语",
        "ru" => "俄语",
        "pt" => "葡萄牙语",
        "es" => "西班牙语",
        "it" => "意大利语",
        other => other,
    }
}

/// 这句话是不是已经是目标语言了。只判断有专用文字的：假名算日语（只有汉字分不出来），谚文算韩语
fn already_in(lang: &str, s: &str) -> bool {
    match lang {
        "ja" => s.chars().any(|c| matches!(c, '\u{3040}'..='\u{30ff}')),
        "ko" => s.chars().any(|c| matches!(c, '\u{ac00}'..='\u{d7a3}')),
        _ => false,
    }
}

/// 模型偶尔会给译文加引号
fn clean_translation(s: &str) -> String {
    s.trim().trim_matches(|c: char| matches!(c, '"' | '“' | '”' | '「' | '」' | '『' | '』')).trim().to_string()
}

/// 要读的译文；没选朗读语言时是 None
async fn translate(cfg: &Config, text: &str) -> Result<Option<String>, String> {
    let Some(lang) = speak_language(cfg) else { return Ok(None) };
    if already_in(lang, text) {
        return Ok(Some(text.to_string()));
    }
    let persona = cfg.active_persona();
    let provider = cfg.aux_provider(&persona).ok_or("选了朗读语言要先配一个对话或辅助模型来翻译")?;
    let name = language_name(lang);
    let system = format!(
        "把桌宠「{}」说的话翻译成自然的{name}口语，用来语音朗读。保持原来的语气和情绪，人名和专有名词用{name}的常见写法。\
只输出译文，不要解释，不要加引号、括号或注音。",
        persona.name
    );
    let messages = [ChatMessage::text("system", system), ChatMessage::text("user", text)];
    let translated = clean_translation(&llm::chat_once(&Endpoint::from_provider(provider), &messages, Some(0.3)).await?);
    if translated.is_empty() {
        return Err("翻译模型没有返回内容".into());
    }
    Ok(Some(translated))
}

// ---------- 引擎 ----------

async fn synthesize(cfg: &Config, text: &str, lang: Option<&str>) -> Result<Vec<u8>, String> {
    match cfg.tts.provider.as_str() {
        "cloud" => cloud(cfg, text).await,
        "local" => local(&cfg.tts, text, lang).await,
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
fn filled(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// 请求内容：选了服务里的角色声音就用它，否则要有参考音频
fn local_body(p: &TtsParams, text: &str, lang: Option<&str>) -> Result<serde_json::Value, String> {
    let mut body = serde_json::json!({
        "text": text,
        "text_language": lang.unwrap_or(p.local_text_lang.trim()),
        "media_type": "wav",
    });
    if let Some(lora) = filled(&p.local_lora) {
        body["lora_id"] = lora.into();
        if let Some(s) = filled(&p.local_speaker) {
            body["speaker"] = s.into();
        }
        return Ok(body);
    }
    body["refer_wav_path"] = filled(&p.local_ref_audio).ok_or("还没有选参考音频或角色声音")?.into();
    if let Some(t) = filled(&p.local_ref_text) {
        body["prompt_text"] = t.into();
        body["prompt_language"] = p.local_ref_lang.trim().into();
    }
    Ok(body)
}

/// 服务里预置的一个角色声音
#[derive(serde::Serialize, Debug, PartialEq, Eq)]
pub struct LocalVoice {
    pub lora: String,
    pub speaker: Option<String>,
}

/// 解析 GET /lora/list 的返回；单说话人的 LoRA 没有 speakers
fn parse_voices(v: &serde_json::Value) -> Option<Vec<LocalVoice>> {
    let mut out = Vec::new();
    for (lora, info) in v.get("loras")?.as_object()? {
        let speakers: Vec<&str> = info.get("speakers").and_then(|s| s.as_array()).map(|a| a.iter().filter_map(|s| s.as_str()).collect()).unwrap_or_default();
        if speakers.is_empty() {
            out.push(LocalVoice { lora: lora.clone(), speaker: None });
        }
        out.extend(speakers.iter().map(|s| LocalVoice { lora: lora.clone(), speaker: Some((*s).to_string()) }));
    }
    Some(out)
}

/// 问本地服务有哪些角色声音（设置页用）
pub async fn local_voices(url: &str) -> Result<Vec<LocalVoice>, String> {
    let url = format!("{}/lora/list", url.trim().trim_end_matches('/'));
    let resp = http().get(&url).timeout(Duration::from_secs(10)).send().await.map_err(|e| format!("连不上本地 TTS 服务：{e}"))?;
    if !resp.status().is_success() {
        return Err("这个服务没有提供角色声音列表，请用参考音频".into());
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("读不懂服务返回的内容：{e}"))?;
    parse_voices(&v).ok_or_else(|| "读不懂服务返回的内容".to_string())
}

async fn local(p: &TtsParams, text: &str, lang: Option<&str>) -> Result<Vec<u8>, String> {
    let url = p.local_url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("本地服务地址要以 http:// 开头，比如 http://127.0.0.1:9880/".into());
    }
    let mut body = local_body(p, text, lang)?;
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

// ---------- 随 Perch 启动本地服务 ----------

/// Perch 启动的服务进程。用户自己开的服务不在这里，退出时也不会动
static SERVICE: Mutex<Option<Child>> = Mutex::new(None);

/// 按设置在后台启动本地服务；服务已经在跑就不管
pub fn autostart_service(app: &AppHandle) {
    let p = app.state::<AppState>().config.lock().unwrap().tts.clone();
    if p.provider != "local" || !p.local_autostart {
        return;
    }
    let Some(script) = filled(&p.local_start_script).map(str::to_string) else { return };
    tauri::async_runtime::spawn(async move {
        // 有任何回应都算服务已经在跑（根路径没带参数时会返回错误，这没关系）
        if http().get(p.local_url.trim()).timeout(Duration::from_millis(1500)).send().await.is_ok() {
            eprintln!("[tts] 本地服务已经在运行");
            return;
        }
        match start_service(Path::new(&script)) {
            Ok(child) => {
                eprintln!("[tts] 已启动本地服务 {script}");
                *SERVICE.lock().unwrap() = Some(child);
            }
            Err(e) => eprintln!("[tts] 启动本地服务失败：{e}"),
        }
    });
}

/// 不开窗口运行脚本，输出写到数据目录的 tts-service.log。stdin 接空，脚本末尾的 pause 不会卡住
fn start_service(script: &Path) -> Result<Child, String> {
    if !script.is_file() {
        return Err(format!("找不到 {}", script.display()));
    }
    let log = std::fs::File::create(crate::config::data_dir().join("tts-service.log")).map_err(|e| e.to_string())?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = service_command(script);
    if let Some(dir) = script.parent() {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null()).stdout(log).stderr(log2).spawn().map_err(|e| e.to_string())
}

#[cfg(windows)]
fn service_command(script: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let ext = script.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    let mut cmd = if ext == "bat" || ext == "cmd" {
        let mut c = Command::new("cmd");
        // cmd /C 会去掉最外层引号，路径有空格时要再包一层
        c.raw_arg(format!("/C \"\"{}\"\"", script.display()));
        c
    } else {
        Command::new(script)
    };
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
fn service_command(script: &Path) -> Command {
    Command::new(script)
}

/// Perch 退出时调用：关掉自己启动的服务，连同它启动的子进程（脚本里的 python）
pub fn stop_service() {
    let Some(mut child) = SERVICE.lock().unwrap().take() else { return };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill").args(["/PID", &child.id().to_string(), "/T", "/F"]).creation_flags(0x0800_0000).output();
    }
    let _ = child.kill();
    let _ = child.wait();
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
    fn local_request_uses_character_voice_or_reference_audio() {
        let mut p = TtsParams::default();
        assert!(local_body(&p, "你好", None).unwrap_err().contains("参考音频"));

        p.local_ref_audio = Some("D:/ref.wav".into());
        p.local_ref_text = Some("参考".into());
        let b = local_body(&p, "你好", None).unwrap();
        assert_eq!((b["refer_wav_path"].as_str(), b["prompt_text"].as_str()), (Some("D:/ref.wav"), Some("参考")));
        assert_eq!(b["text_language"].as_str(), Some("zh"));
        assert!(b.get("lora_id").is_none());

        // 选了角色声音就不发参考音频
        p.local_lora = Some("roselia_qwen3tts".into());
        p.local_speaker = Some("yukina".into());
        let b = local_body(&p, "こんにちは", Some("ja")).unwrap();
        assert_eq!(b["text_language"].as_str(), Some("ja"));
        assert_eq!((b["lora_id"].as_str(), b["speaker"].as_str()), (Some("roselia_qwen3tts"), Some("yukina")));
        assert!(b.get("refer_wav_path").is_none() && b.get("prompt_text").is_none());
    }

    #[test]
    fn recognises_target_language_and_cleans_translation() {
        assert!(already_in("ja", "続けましょう") && already_in("ja", "カタカナ"));
        assert!(!already_in("ja", "继续吧") && !already_in("ja", "hello"));
        assert!(already_in("ko", "안녕하세요") && !already_in("ko", "继续吧"));
        assert!(!already_in("en", "hello"));
        assert_eq!(language_name("en"), "英语");
        assert_eq!(language_name("nl"), "nl");
        assert_eq!(clean_translation(" 「続けましょう」\n"), "続けましょう");
    }

    #[test]
    fn service_is_off_by_default_and_reports_missing_script() {
        let p = TtsParams::default();
        assert!(!p.local_autostart && p.local_start_script.is_none());
        assert!(start_service(Path::new("D:/没有这个/start.bat")).unwrap_err().contains("找不到"));
    }

    #[test]
    fn parses_voice_list() {
        let v = serde_json::json!({ "active_lora_id": null, "loras": {
            "roselia": { "speakers": ["yukina", "lisa"], "multi_speaker": true },
            "solo": { "speakers": [], "multi_speaker": false },
        }});
        let voices = parse_voices(&v).unwrap();
        assert_eq!(voices.len(), 3);
        assert_eq!(voices[0], LocalVoice { lora: "roselia".into(), speaker: Some("yukina".into()) });
        assert_eq!(voices[2], LocalVoice { lora: "solo".into(), speaker: None });
        assert!(parse_voices(&serde_json::json!({ "code": 400 })).is_none());
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
