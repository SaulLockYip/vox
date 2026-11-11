//! 麦克风采集（cpal 0.15）。
//!
//! 采集期间把各声道混成单声道 f32 并累积到共享缓冲；停止后重采样到 16 kHz。
//!
//! cpal 0.15 的 `Device` 不再暴露 id，只能按**名称**选择设备。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use vox_core::error::{Error, Result};

use crate::resample::resample;
use crate::TARGET_SAMPLE_RATE;

/// 一段录音（已混成单声道）。
#[derive(Debug, Clone)]
pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Recording {
    /// 重采样到 ASR 需要的 16 kHz 单声道。
    pub fn to_target_rate(&self) -> Recording {
        Recording {
            samples: resample(&self.samples, self.sample_rate, TARGET_SAMPLE_RATE),
            sample_rate: TARGET_SAMPLE_RATE,
        }
    }

    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples.len() as f64 / self.sample_rate as f64
        }
    }
}

/// 列出可用的输入设备名称。
pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|e| Error::Audio(format!("枚举输入设备失败: {e}")))?;
    let mut out = Vec::new();
    for device in devices {
        let name = device
            .name()
            .unwrap_or_else(|_| "(未知设备)".to_string());
        out.push(name);
    }
    Ok(out)
}

/// 正在录音的会话。
pub struct Recorder {
    stop: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
    stream: Option<Stream>,
    sample_rate: u32,
}

impl Recorder {
    /// 开始录音。`device_name` 为 `None` 时用系统默认输入设备。
    pub fn start(device_name: Option<&str>) -> Result<Recorder> {
        let host = cpal::default_host();

        let device = match device_name {
            Some(name) => {
                let mut devices = host
                    .input_devices()
                    .map_err(|e| Error::Audio(format!("枚举输入设备失败: {e}")))?;
                devices
                    .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                    .ok_or_else(|| Error::Audio(format!("找不到输入设备: {name}")))?
            }
            None => host.default_input_device().ok_or(Error::NoInputDevice)?,
        };

        let supported = device
            .default_input_config()
            .map_err(|e| Error::Audio(format!("获取默认输入配置失败: {e}")))?;

        // 采样格式只在 SupportedStreamConfig 上有；StreamConfig 的格式由回调类型决定
        let sample_format = supported.sample_format();
        let channels = supported.channels().max(1) as usize;
        let sample_rate = supported.sample_rate().0;
        let config = supported.config();
        let stop = Arc::new(AtomicBool::new(false));
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let err_fn = |err: cpal::StreamError| tracing::warn!(error = %err, "音频流错误");

        let stop_c = Arc::clone(&stop);
        let buf_c = Arc::clone(&buffer);

        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    push_mono_f32(data, channels, &buf_c, &stop_c);
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let tmp: Vec<f32> = data.iter().map(|v| *v as f32 / 32768.0).collect();
                    push_mono_f32(&tmp, channels, &buf_c, &stop_c);
                },
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let tmp: Vec<f32> = data
                        .iter()
                        .map(|v| (*v as f32 - 32768.0) / 32768.0)
                        .collect();
                    push_mono_f32(&tmp, channels, &buf_c, &stop_c);
                },
                err_fn,
                None,
            ),
            SampleFormat::I32 => device.build_input_stream(
                &config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    let tmp: Vec<f32> = data.iter().map(|v| *v as f32 / 2147483648.0).collect();
                    push_mono_f32(&tmp, channels, &buf_c, &stop_c);
                },
                err_fn,
                None,
            ),
            other => {
                return Err(Error::Audio(format!(
                    "不支持的采样格式: {other:?}（请在系统里改成 16/24 bit 或 float）"
                )))
            }
        }
        .map_err(|e| Error::Audio(format!("创建音频流失败: {e}")))?;

        stream
            .play()
            .map_err(|e| Error::Audio(format!("启动音频流失败: {e}")))?;

        Ok(Recorder {
            stop,
            buffer,
            stream: Some(stream),
            sample_rate,
        })
    }

    /// 停止录音并取回数据。
    pub fn stop(mut self) -> Result<Recording> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(stream) = self.stream.take() {
            // 丢弃 stream 即停止回调
            drop(stream);
        }
        let samples = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|_| Error::Audio("音频缓冲锁异常".into()))?;
            std::mem::take(&mut *buf)
        };
        Ok(Recording {
            samples,
            sample_rate: self.sample_rate,
        })
    }

    /// 当前已采集的秒数（用于 UI 波形与时长显示）。
    pub fn elapsed_secs(&self) -> f64 {
        let len = self.buffer.lock().map(|b| b.len()).unwrap_or(0);
        if self.sample_rate == 0 {
            0.0
        } else {
            len as f64 / self.sample_rate as f64
        }
    }
}

/// 把多声道交错采样混成单声道并追加到缓冲。
fn push_mono_f32(
    data: &[f32],
    channels: usize,
    buffer: &Arc<Mutex<Vec<f32>>>,
    stop: &Arc<AtomicBool>,
) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let mut mono = Vec::with_capacity(data.len() / channels);
    if channels == 1 {
        mono.extend_from_slice(data);
    } else {
        let frames = data.len() / channels;
        for f in 0..frames {
            let mut sum = 0f32;
            for c in 0..channels {
                sum += data[f * channels + c];
            }
            mono.push(sum / channels as f32);
        }
    }
    if let Ok(mut buf) = buffer.lock() {
        buf.extend_from_slice(&mono);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_channels_to_mono() {
        let data = vec![1.0f32, 3.0, 5.0, 7.0]; // 2 帧 × 2 声道
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        push_mono_f32(&data, 2, &buffer, &stop);
        let out = buffer.lock().unwrap().clone();
        assert_eq!(out, vec![2.0, 6.0]);
    }

    #[test]
    fn single_channel_passthrough() {
        let data = vec![0.5f32, 0.25];
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        push_mono_f32(&data, 1, &buffer, &stop);
        assert_eq!(buffer.lock().unwrap().clone(), vec![0.5, 0.25]);
    }

    #[test]
    fn recording_duration() {
        let r = Recording {
            samples: vec![0.0; 16_000],
            sample_rate: 16_000,
        };
        assert!((r.duration_secs() - 1.0).abs() < 1e-9);
    }
}
