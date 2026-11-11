//! WAV 编码（16 bit PCM 单声道）。

use std::io::Cursor;

use vox_core::error::{Error, Result};

/// 把 f32 采样编码成 WAV 字节流。
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| Error::Audio(format!("创建 WAV 写入器失败: {e}")))?;
        for s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            let v = (clamped * i16::MAX as f32) as i16;
            writer
                .write_sample(v)
                .map_err(|e| Error::Audio(format!("写入 WAV 采样失败: {e}")))?;
        }
        writer
            .finalize()
            .map_err(|e| Error::Audio(format!("收尾 WAV 失败: {e}")))?;
    }
    Ok(cursor.into_inner())
}

/// 把 WAV 写到文件。
pub fn write_wav(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let bytes = encode_wav(samples, sample_rate)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_valid_wav_header() {
        let samples = vec![0.0f32; 16_000];
        let bytes = encode_wav(&samples, 16_000).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        // data 段长度 = 16000 * 2
        let data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        assert_eq!(data_len, 32_000);
    }

    #[test]
    fn clips_out_of_range_samples() {
        let bytes = encode_wav(&[2.0, -2.0], 16_000).unwrap();
        let s1 = i16::from_le_bytes(bytes[44..46].try_into().unwrap());
        let s2 = i16::from_le_bytes(bytes[46..48].try_into().unwrap());
        assert_eq!(s1, i16::MAX);
        assert_eq!(s2, -i16::MAX);
    }
}
