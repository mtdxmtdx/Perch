//! 本地语音输入：按住说话 → 麦克风录音 → Silero VAD 去掉静音 → SenseVoice 离线识别 → 直接发送（或填进输入框）。
//! 可选功能，默认关闭；开启后才加载语音运行库（见 runtime.rs），不开时程序不需要那些 DLL。

pub mod asr;
pub mod mic;
pub mod models;
pub mod runtime;

use crate::{chat, windows, AppState};
use serde::Serialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

#[derive(Default)]
pub struct VoiceRuntime {
    recorder: Mutex<Option<mic::Recorder>>,
    /// 已加载的识别器，以及它对应的模型文件（换了模型目录要重新加载）
    asr: Mutex<Option<(PathBuf, Arc<asr::Asr>)>>,
    downloading: AtomicBool,
    cancel_download: AtomicBool,
    /// 快捷键注册失败的原因（给设置页显示）
    hotkey_error: Mutex<Option<String>>,
    /// 每次录音一个编号：实时预览循环发现编号变了（已松开）就停，晚到的预览结果也会被丢掉
    take: AtomicU64,
}

/// 录音时每隔多久重新识别一次已录到的部分，做实时预览
const PREVIEW_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
/// 至少录到这么长才开始预览
const PREVIEW_MIN_SECONDS: f32 = 0.6;

/// 广播给气泡和设置页的语音状态
#[derive(Serialize, Clone)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum VoiceEvent {
    Recording,
    Level { level: f32 },
    /// 录音过程中的实时识别结果（说到哪儿识别到哪儿）
    Partial { text: String },
    Recognizing,
    Idle,
    /// sent=false 时前端把文字填进输入框
    Result { text: String, sent: bool },
    Error { error: String },
}

fn emit(app: &AppHandle, ev: VoiceEvent) {
    let _ = app.emit("voice", ev);
}

pub fn report_error(app: &AppHandle, error: String) {
    emit(app, VoiceEvent::Error { error });
}

fn model_paths(app: &AppHandle) -> Result<models::ModelPaths, String> {
    let dir = app.state::<AppState>().config.lock().unwrap().voice.model_dir.clone();
    models::locate(dir.as_deref()).map_err(|_| "语音模型还没下载，请到 设置 → 语音 里下载".to_string())
}

/// 取已加载的识别器，没有就加载（约 1~2 秒，放在阻塞线程里）
async fn load_asr(app: &AppHandle) -> Result<Arc<asr::Asr>, String> {
    let paths = model_paths(app)?;
    let state = app.state::<AppState>();
    if let Some((key, asr)) = state.voice.asr.lock().unwrap().as_ref() {
        if *key == paths.sense_voice {
            return Ok(asr.clone());
        }
    }
    let p = paths.clone();
    let asr = tauri::async_runtime::spawn_blocking(move || asr::Asr::load(&p))
        .await
        .map_err(|e| e.to_string())??;
    let asr = Arc::new(asr);
    *state.voice.asr.lock().unwrap() = Some((paths.sense_voice, asr.clone()));
    Ok(asr)
}

/// 开始录音（快捷键按下 / 按住 🎤）。已经在录就忽略（按住快捷键时会连续触发）
pub fn start(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut rec = state.voice.recorder.lock().unwrap();
    if rec.is_some() {
        return Ok(());
    }
    let (enabled, runtime_dir) = {
        let cfg = state.config.lock().unwrap();
        (cfg.voice.enabled, cfg.voice.runtime_dir.clone())
    };
    if !enabled {
        return Err("语音输入没有开启，可以到 设置 → 语音 里打开".into());
    }
    // 调用任何 sherpa 接口之前先把运行库加载进来
    runtime::ensure_loaded(runtime_dir.as_deref())?;
    // 开始说话了：打断正在读的
    crate::tts::stop();
    model_paths(app)?;
    let device = state.config.lock().unwrap().voice.device.clone();
    let app2 = app.clone();
    let r = mic::start(device, move |level| emit(&app2, VoiceEvent::Level { level }))?;
    *rec = Some(r);
    drop(rec);
    let take = state.voice.take.fetch_add(1, Ordering::SeqCst) + 1;

    // 在聊天窗口里按住说话时不弹气泡；其余情况（快捷键）弹出气泡显示录音状态
    let bubble_visible = state.shared.lock().unwrap().bubble_visible;
    let chat_focused = app
        .get_webview_window(windows::CHAT)
        .map(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
        .unwrap_or(false);
    if !bubble_visible && !chat_focused {
        windows::open_bubble(app, "input");
    }
    chat::set_speech(app, Some("listen"), None);
    emit(app, VoiceEvent::Recording);
    // 趁说话的时间把模型加载好（第一次用时），然后边录边识别做实时预览
    let app3 = app.clone();
    tauri::async_runtime::spawn(async move {
        let asr = match load_asr(&app3).await {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[voice] 预加载模型失败：{e}");
                return;
            }
        };
        preview_loop(&app3, take, asr).await;
    });
    Ok(())
}

/// 正在录音（主动聊天据此判断能不能打扰）
pub fn is_recording(app: &AppHandle) -> bool {
    app.state::<AppState>().voice.recorder.lock().unwrap().is_some()
}

fn still_recording(app: &AppHandle, take: u64) -> bool {
    let state = app.state::<AppState>();
    state.voice.take.load(Ordering::SeqCst) == take && state.voice.recorder.lock().unwrap().is_some()
}

async fn preview_loop(app: &AppHandle, take: u64, asr: Arc<asr::Asr>) {
    let mut last = String::new();
    loop {
        tokio::time::sleep(PREVIEW_EVERY).await;
        if !still_recording(app, take) {
            return;
        }
        let snap = app.state::<AppState>().voice.recorder.lock().unwrap().as_ref().map(|r| r.snapshot());
        let Some(cap) = snap else { return };
        if (cap.samples.len() as f32) < cap.rate as f32 * PREVIEW_MIN_SECONDS {
            continue;
        }
        let a = asr.clone();
        let text = tauri::async_runtime::spawn_blocking(move || a.transcribe(&asr::to_16k(&cap.samples, cap.rate)))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        // 识别期间松开了：这份结果已经过时，交给最终识别
        if !still_recording(app, take) {
            return;
        }
        let text = text.trim().to_string();
        if !text.is_empty() && text != last {
            last = text.clone();
            emit(app, VoiceEvent::Partial { text });
        }
    }
}

/// 结束录音并识别（快捷键松开 / 松开 🎤）。send 为 None 时按设置决定是否直接发送
pub async fn stop(app: &AppHandle, send: Option<bool>) -> Result<String, String> {
    let state = app.state::<AppState>();
    let Some(rec) = state.voice.recorder.lock().unwrap().take() else { return Ok(String::new()) };
    // 让实时预览立刻停下
    state.voice.take.fetch_add(1, Ordering::SeqCst);
    emit(app, VoiceEvent::Recognizing);
    chat::set_speech(app, Some("think"), None);

    let result: Result<String, String> = async {
        let cap = tauri::async_runtime::spawn_blocking(move || rec.finish()).await.map_err(|e| e.to_string())??;
        let asr = load_asr(app).await?;
        tauri::async_runtime::spawn_blocking(move || asr.transcribe(&asr::to_16k(&cap.samples, cap.rate)))
            .await
            .map_err(|e| e.to_string())?
    }
    .await;
    chat::set_speech(app, None, None);

    let text = match result {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            emit(app, VoiceEvent::Error { error: e.clone() });
            return Err(e);
        }
    };
    if text.is_empty() {
        emit(app, VoiceEvent::Error { error: "没听清，再说一次？".into() });
        return Ok(text);
    }
    let send = send.unwrap_or_else(|| state.config.lock().unwrap().voice.auto_send);
    emit(app, VoiceEvent::Result { text: text.clone(), sent: send });
    if send {
        let (app2, t) = (app.clone(), text.clone());
        tauri::async_runtime::spawn(async move {
            // 发送前就失败的（比如上一条还没回完）不会有对话事件，在气泡里提示
            if let Err(e) = chat::send(app2.clone(), t, Vec::new()).await {
                if e.contains("还没回完") || e.contains("配置") {
                    emit(&app2, VoiceEvent::Error { error: e });
                }
            }
        });
    }
    Ok(text)
}

/// 放弃这次录音
pub fn cancel(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Some(rec) = state.voice.recorder.lock().unwrap().take() {
        state.voice.take.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || drop(rec.finish()));
    }
    chat::set_speech(app, None, None);
    emit(app, VoiceEvent::Idle);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// 找到了语音运行库
    pub runtime_ready: bool,
    pub runtime_dir: Option<String>,
    /// 这次运行已经从哪里加载了运行库（换文件夹要重启才生效）
    pub runtime_loaded_from: Option<String>,
    pub runtime_download_dir: String,
    pub model_ready: bool,
    pub missing: Vec<String>,
    pub downloading: bool,
    pub download_dir: String,
    pub devices: Vec<String>,
    pub hotkey_error: Option<String>,
}

pub fn status(app: &AppHandle) -> Status {
    let state = app.state::<AppState>();
    let (dir, runtime_dir) = {
        let cfg = state.config.lock().unwrap();
        (cfg.voice.model_dir.clone(), cfg.voice.runtime_dir.clone())
    };
    let located = models::locate(dir.as_deref());
    let hotkey_error = state.voice.hotkey_error.lock().unwrap().clone();
    let runtime_found = runtime::locate(runtime_dir.as_deref());
    Status {
        runtime_ready: runtime_found.is_some(),
        runtime_dir: runtime_found.map(|d| d.display().to_string()),
        runtime_loaded_from: runtime::loaded_from().map(|d| d.display().to_string()),
        runtime_download_dir: runtime::default_dir().display().to_string(),
        model_ready: located.is_ok(),
        missing: located.err().unwrap_or_default(),
        downloading: state.voice.downloading.load(Ordering::SeqCst),
        download_dir: models::default_dir().display().to_string(),
        devices: mic::list_devices(),
        hotkey_error,
    }
}

/// 下载语音组件：缺运行库先下运行库，再下缺少的模型；进度通过 "voice-download" 事件发出
pub async fn download(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if state.voice.downloading.swap(true, Ordering::SeqCst) {
        return Err("正在下载中".into());
    }
    state.voice.cancel_download.store(false, Ordering::SeqCst);
    let runtime_dir = state.config.lock().unwrap().voice.runtime_dir.clone();
    let progress = |p: models::Progress| {
        let _ = app.emit("voice-download", p);
    };
    let r = async {
        if runtime::locate(runtime_dir.as_deref()).is_none() {
            models::download_runtime(&state.voice.cancel_download, progress).await?;
        }
        models::download_all(&state.voice.cancel_download, progress).await
    }
    .await;
    state.voice.downloading.store(false, Ordering::SeqCst);
    let _ = app.emit("voice-download-done", r.as_ref().err().cloned());
    r
}

pub fn cancel_download(app: &AppHandle) {
    app.state::<AppState>().voice.cancel_download.store(true, Ordering::SeqCst);
}

/// 检查快捷键写法
pub fn parse_hotkey(s: &str) -> Result<Option<Shortcut>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    Shortcut::from_str(s).map(Some).map_err(|e| format!("快捷键「{s}」格式不对：{e}。例子：Ctrl+Shift+Space、Alt+Q、F9"))
}

/// 按配置重新注册按住说话的快捷键
pub fn register_hotkey(app: &AppHandle) -> Result<(), String> {
    let (enabled, hotkey) = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap();
        (cfg.voice.enabled, cfg.voice.hotkey.clone())
    };
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    // 语音输入关着时不占用快捷键
    if !enabled {
        *app.state::<AppState>().voice.hotkey_error.lock().unwrap() = None;
        return Ok(());
    }
    let r = parse_hotkey(&hotkey).and_then(|sc| match sc {
        Some(sc) => gs.register(sc).map_err(|e| format!("快捷键「{hotkey}」注册失败（可能被别的程序占用了）：{e}")),
        None => Ok(()),
    });
    *app.state::<AppState>().voice.hotkey_error.lock().unwrap() = r.as_ref().err().cloned();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 链接检查：sherpa-onnx 的原生库能链接，接口能调用（模型路径不存在时返回 None）
    #[test]
    fn sherpa_links() {
        let mut cfg = sherpa_onnx::OfflineRecognizerConfig::default();
        cfg.model_config.sense_voice.model = Some("不存在.onnx".into());
        cfg.model_config.tokens = Some("不存在.txt".into());
        assert!(sherpa_onnx::OfflineRecognizer::create(&cfg).is_none());
        let r = sherpa_onnx::LinearResampler::create(48000, 16000).unwrap();
        assert_eq!(r.resample(&vec![0.0; 4800], true).len(), 1600);
    }

    /// 真实下载模型并识别一段录音（耗时、要联网，默认跳过）。
    /// 运行：PERCH_DATA_DIR=<测试目录> TP_TEST_WAV=<16k wav> cargo test download_and_transcribe -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn download_and_transcribe() {
        let cancel = AtomicBool::new(false);
        let mut last = String::new();
        models::download_all(&cancel, |p| {
            let line = format!("{} {}MB/{}MB 来自 {}", p.file, p.downloaded >> 20, p.total >> 20, p.source);
            if line != last {
                eprintln!("{line}");
                last = line;
            }
        })
        .await
        .unwrap();
        let paths = models::locate(None).unwrap();
        let t = std::time::Instant::now();
        let asr = asr::Asr::load(&paths).unwrap();
        eprintln!("加载模型 {:?}", t.elapsed());
        let wav = sherpa_onnx::Wave::read(&std::env::var("TP_TEST_WAV").unwrap()).unwrap();
        let t = std::time::Instant::now();
        let text = asr.transcribe(&asr::to_16k(wav.samples(), wav.sample_rate() as u32)).unwrap();
        eprintln!("识别 {:?}：{text}", t.elapsed());
        assert!(!text.is_empty());
        // 只有静音：返回空
        assert_eq!(asr.transcribe(&vec![0.0; 16000 * 2]).unwrap(), "");

        // 模拟常见的麦克风格式：48kHz、双声道、i16。走一遍程序里的"混成单声道 → 降到 16k"，结果应该一样
        let src = asr::to_16k(wav.samples(), wav.sample_rate() as u32);
        let up = sherpa_onnx::LinearResampler::create(16000, 48000).unwrap().resample(&src, true);
        let stereo_i16: Vec<i16> = up.iter().flat_map(|&s| {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            [v, v]
        }).collect();
        let buf = std::sync::Mutex::new(Vec::new());
        for block in stereo_i16.chunks(960) {
            mic::push_mono_for_test(block, 2, &buf);
        }
        let mono48k = buf.into_inner().unwrap();
        let text48 = asr.transcribe(&asr::to_16k(&mono48k, 48000)).unwrap();
        eprintln!("48kHz 立体声 i16 → 识别：{text48}");
        // 重采样会让波形有细微差别，同音字可能判得不同（"开饭/开放"），不要求逐字相同
        assert!(text48.contains("早上九点至下午五点"));
        let (a, b): (Vec<char>, Vec<char>) = (text.chars().collect(), text48.chars().collect());
        let diff = a.len().abs_diff(b.len()) + a.iter().zip(&b).filter(|(x, y)| x != y).count();
        assert!(diff <= 2, "两条路径的结果差太多：{text} / {text48}");
    }

    /// 模拟实时预览：按 PREVIEW_EVERY 的节奏截取越来越长的录音识别，看每次耗时和结果（要已下载模型，默认跳过）。
    /// 运行：TP_TEST_WAV=<wav> cargo test preview_partials -- --ignored --nocapture
    #[test]
    #[ignore]
    fn preview_partials() {
        let asr = asr::Asr::load(&models::locate(None).unwrap()).unwrap();
        let wav = sherpa_onnx::Wave::read(&std::env::var("TP_TEST_WAV").unwrap()).unwrap();
        let rate = wav.sample_rate() as u32;
        let step = (rate as f32 * PREVIEW_EVERY.as_secs_f32()) as usize;
        let mut end = step;
        let mut partials = Vec::new();
        while end < wav.samples().len() + step {
            let cut = &wav.samples()[..end.min(wav.samples().len())];
            if cut.len() as f32 >= rate as f32 * PREVIEW_MIN_SECONDS {
                let t = std::time::Instant::now();
                let text = asr.transcribe(&asr::to_16k(cut, rate)).unwrap();
                let took = t.elapsed();
                eprintln!("{:>5.1}s 录音 → {:?} 识别：{text}", cut.len() as f32 / rate as f32, took);
                // 识别要比预览间隔快，预览才跟得上说话（慢了也不会堆积，只是更新变稀）
                assert!(took < PREVIEW_EVERY, "识别太慢：{took:?}");
                partials.push(text);
            }
            end += step;
        }
        // 预览逐渐变长，最后一次就是完整结果
        assert!(partials.iter().filter(|t| !t.is_empty()).count() >= 2);
        assert!(partials.last().unwrap().contains("早上九点至下午五点"));
    }

    #[test]
    fn hotkey_parsing() {
        assert!(parse_hotkey("Ctrl+Shift+Space").unwrap().is_some());
        assert!(parse_hotkey("F9").unwrap().is_some());
        assert!(parse_hotkey("  ").unwrap().is_none());
        assert!(parse_hotkey("Ctrl+不存在的键").is_err());
    }
}
