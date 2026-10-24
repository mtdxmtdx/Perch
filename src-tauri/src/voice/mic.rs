//! 麦克风录音：在独立线程里跑 cpal 输入流（Windows 上 Stream 不能跨线程），转成单声道 f32。

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// 最长录 60 秒，防止忘了松开
const MAX_SECONDS: u32 = 60;

pub struct Captured {
    pub samples: Vec<f32>,
    pub rate: u32,
}

pub struct Recorder {
    stop: mpsc::Sender<()>,
    handle: JoinHandle<Result<Captured, String>>,
    /// 录音线程写入的缓冲区，录音过程中也能取一份做实时预览
    buf: Arc<Mutex<Vec<f32>>>,
    rate: u32,
}

impl Recorder {
    /// 到目前为止录到的音频（拷贝一份，不打断录音）
    pub fn snapshot(&self) -> Captured {
        Captured { samples: self.buf.lock().unwrap().clone(), rate: self.rate }
    }

    /// 停止录音，取回录到的音频
    pub fn finish(self) -> Result<Captured, String> {
        let _ = self.stop.send(());
        self.handle.join().map_err(|_| "录音线程异常退出".to_string())?
    }
}

pub fn list_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices().map(|ds| ds.map(|d| d.to_string()).collect()).unwrap_or_default()
}

fn pick_device(name: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        if let Ok(mut ds) = host.input_devices() {
            if let Some(d) = ds.find(|d| d.to_string() == name) {
                return Ok(d);
            }
        }
        // 指定的麦克风不在了（拔掉了），退回默认
    }
    host.default_input_device().ok_or_else(|| "没有找到麦克风".to_string())
}

/// 把交错的多声道采样混成单声道，追加到 buf；返回这一块的音量（RMS）
fn push_mono<T: SizedSample>(data: &[T], channels: usize, to_f32: fn(T) -> f32, buf: &Mutex<Vec<f32>>) -> f32 {
    let mut out = buf.lock().unwrap();
    let mut sum_sq = 0.0f32;
    let mut n = 0usize;
    for frame in data.chunks(channels.max(1)) {
        let v = frame.iter().map(|&s| to_f32(s)).sum::<f32>() / frame.len() as f32;
        sum_sq += v * v;
        n += 1;
        out.push(v);
    }
    if n == 0 { 0.0 } else { (sum_sq / n as f32).sqrt() }
}

/// 开始录音。on_level 大约每 50ms 收到一次 0~1 的音量，给界面画电平条
pub fn start(device: Option<String>, on_level: impl Fn(f32) + Send + 'static) -> Result<Recorder, String> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(Arc<Mutex<Vec<f32>>>, u32), String>>();
    let handle = std::thread::spawn(move || -> Result<Captured, String> {
        let dev = match pick_device(device.as_deref()) {
            Ok(d) => d,
            Err(e) => {
                let _ = ready_tx.send(Err(e.clone()));
                return Err(e);
            }
        };
        let supported = dev.default_input_config().map_err(|e| format!("读取麦克风格式失败：{e}"))?;
        let rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        let buf = Arc::new(Mutex::new(Vec::<f32>::with_capacity(rate as usize * 10)));
        let level = Arc::new(Mutex::new(0.0f32));
        let config = supported.config();
        let err_fn = |e| eprintln!("[voice] 录音出错：{e}");

        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let (b, l) = (buf.clone(), level.clone());
                dev.build_input_stream::<$t, _, _>(
                    config.clone(),
                    move |data: &[$t], _| {
                        let rms = push_mono(data, channels, $conv, &b);
                        let mut lv = l.lock().unwrap();
                        *lv = lv.max(rms);
                    },
                    err_fn,
                    None,
                )
            }};
        }
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build!(f32, |s| s),
            SampleFormat::I16 => build!(i16, |s| s as f32 / 32768.0),
            SampleFormat::U16 => build!(u16, |s| (s as f32 - 32768.0) / 32768.0),
            SampleFormat::I32 => build!(i32, |s| s as f32 / 2_147_483_648.0),
            other => {
                let e = format!("不支持的麦克风采样格式 {other:?}");
                let _ = ready_tx.send(Err(e.clone()));
                return Err(e);
            }
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                let e = format!("打开麦克风失败：{e}（检查 Windows 设置 → 隐私 → 麦克风 是否允许桌面应用使用）");
                let _ = ready_tx.send(Err(e.clone()));
                return Err(e);
            }
        };
        if let Err(e) = stream.play() {
            let e = format!("启动录音失败：{e}");
            let _ = ready_tx.send(Err(e.clone()));
            return Err(e);
        }
        let _ = ready_tx.send(Ok((buf.clone(), rate)));

        let limit = rate as usize * MAX_SECONDS as usize;
        loop {
            match stop_rx.recv_timeout(Duration::from_millis(50)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                _ => break,
            }
            let lv = std::mem::take(&mut *level.lock().unwrap());
            // 说话时 RMS 大约 0.02~0.3，放大一些方便显示
            on_level((lv * 6.0).min(1.0));
            if buf.lock().unwrap().len() >= limit {
                break;
            }
        }
        drop(stream);
        let samples = std::mem::take(&mut *buf.lock().unwrap());
        Ok(Captured { samples, rate })
    });
    // 等麦克风真正打开，打不开就立即报错
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok((buf, rate))) => Ok(Recorder { stop: stop_tx, handle, buf, rate }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("打开麦克风超时".into()),
    }
}

/// 给集成测试用：按 i16 麦克风的方式混成单声道
#[cfg(test)]
pub fn push_mono_for_test(data: &[i16], channels: usize, buf: &Mutex<Vec<f32>>) {
    push_mono(data, channels, |s| s as f32 / 32768.0, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_to_mono_and_measures_level() {
        let buf = Mutex::new(Vec::new());
        let rms = push_mono(&[0.5f32, -0.5, 1.0, 0.0], 2, |s| s, &buf);
        assert_eq!(*buf.lock().unwrap(), vec![0.0, 0.5]);
        assert!((rms - (0.125f32).sqrt()).abs() < 1e-6);
        let buf = Mutex::new(Vec::new());
        push_mono(&[16384i16, -16384], 1, |s| s as f32 / 32768.0, &buf);
        assert_eq!(*buf.lock().unwrap(), vec![0.5, -0.5]);
    }
}
