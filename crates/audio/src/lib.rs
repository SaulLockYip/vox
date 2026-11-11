//! 麦克风采集、重采样与 WAV 编码。

pub mod recorder;
pub mod resample;
pub mod wav;

pub use recorder::{list_input_devices, Recorder};
pub use resample::resample;
pub use wav::{encode_wav, write_wav};

/// ASR 要求的采样率（16 kHz 单声道）。
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
