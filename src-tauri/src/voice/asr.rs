//! 识别：Silero VAD 切出说话的片段（去掉前后和中间的静音），再逐段交给 SenseVoice。

use super::models::ModelPaths;
use sherpa_onnx::{
    LinearResampler, OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, SileroVadModelConfig,
    VadModelConfig, VoiceActivityDetector,
};
use std::path::PathBuf;

pub const SAMPLE_RATE: i32 = 16_000;
/// 太短的录音（误触）直接忽略
const MIN_SECONDS: f32 = 0.3;

pub struct Asr {
    recognizer: OfflineRecognizer,
    vad_model: PathBuf,
}

impl Asr {
    /// 加载模型（约 1~2 秒），加载一次后常驻
    pub fn load(paths: &ModelPaths) -> Result<Self, String> {
        let mut cfg = OfflineRecognizerConfig::default();
        cfg.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(paths.sense_voice.to_string_lossy().into()),
            language: Some("auto".into()),
            // 自动加标点、把"一百二十"之类转成数字
            use_itn: true,
        };
        cfg.model_config.tokens = Some(paths.tokens.to_string_lossy().into());
        cfg.model_config.num_threads = std::thread::available_parallelism().map(|n| n.get().min(4) as i32).unwrap_or(2);
        let recognizer = OfflineRecognizer::create(&cfg).ok_or("加载 SenseVoice 模型失败，模型文件可能不完整")?;
        Ok(Self { recognizer, vad_model: paths.vad.clone() })
    }

    /// 输入 16kHz 单声道；没听到说话时返回空字符串
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        if (samples.len() as f32) < SAMPLE_RATE as f32 * MIN_SECONDS {
            return Ok(String::new());
        }
        let mut text = String::new();
        for seg in self.speech_segments(samples)? {
            let stream = self.recognizer.create_stream();
            stream.accept_waveform(SAMPLE_RATE, &seg);
            self.recognizer.decode(&stream);
            if let Some(r) = stream.get_result() {
                text.push_str(r.text.trim());
            }
        }
        Ok(text)
    }

    fn speech_segments(&self, samples: &[f32]) -> Result<Vec<Vec<f32>>, String> {
        let cfg = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(self.vad_model.to_string_lossy().into()),
                threshold: 0.5,
                min_silence_duration: 0.4,
                min_speech_duration: 0.2,
                window_size: 512,
                max_speech_duration: 20.0,
            },
            sample_rate: SAMPLE_RATE,
            num_threads: 1,
            ..Default::default()
        };
        let vad = VoiceActivityDetector::create(&cfg, 120.0).ok_or("加载 VAD 模型失败")?;
        let mut segs = Vec::new();
        let mut drain = |vad: &VoiceActivityDetector| {
            while !vad.is_empty() {
                if let Some(s) = vad.front() {
                    segs.push(s.samples().to_vec());
                }
                vad.pop();
            }
        };
        for chunk in samples.chunks(512) {
            vad.accept_waveform(chunk);
            drain(&vad);
        }
        vad.flush();
        drain(&vad);
        Ok(segs)
    }
}

/// 把麦克风的原始采样率转成 16kHz
pub fn to_16k(samples: &[f32], rate: u32) -> Vec<f32> {
    if rate as i32 == SAMPLE_RATE {
        return samples.to_vec();
    }
    match LinearResampler::create(rate as i32, SAMPLE_RATE) {
        Some(r) => r.resample(samples, true),
        None => samples.to_vec(),
    }
}
