//! 录音开始/结束音效。
//!
//! 不打包音频资源：首次使用时**现场合成**两个短 WAV（开始=上行双音，结束=下行双音），
//! 写到 `<app_dir>/sounds/` 下，再用 AudioToolbox 以系统音效 ID 播放
//! （走系统输出设备，不需要自己管 cpal 的输出流）。
//!
//! 合成参数：16-bit PCM / 单声道 / 44100Hz。

use std::ffi::{c_void, CString};
use std::path::PathBuf;
use std::sync::OnceLock;

use vox_core::config::Config;

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFURLRef = *const c_void;

/// `kCFStringEncodingUTF8`
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
/// `kCFURLPOSIXPathStyle`
const CF_URL_POSIX_PATH_STYLE: i32 = 0;

const SAMPLE_RATE: u32 = 44100;

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioServicesCreateSystemSoundID(url: CFURLRef, out_id: *mut u32) -> i32;
    fn AudioServicesPlaySystemSound(id: u32);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: CFTypeRef,
        c_str: *const std::os::raw::c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFURLCreateWithFileSystemPath(
        alloc: CFTypeRef,
        path: CFStringRef,
        style: i32,
        is_dir: bool,
    ) -> CFURLRef;
    fn CFRelease(cf: CFTypeRef);
}

/// 两个音效的系统 ID（进程内缓存，只创建一次）。
struct SoundIds {
    start: u32,
    stop: u32,
}

static IDS: OnceLock<Option<SoundIds>> = OnceLock::new();

/// 播放「开始录音」音效。
pub fn play_start() {
    play(true);
}

/// 播放「结束录音」音效。
pub fn play_stop() {
    play(false);
}

fn play(is_start: bool) {
    let ids = IDS.get_or_init(|| {
        let start = load_or_create("start", &[880.0, 1320.0]);
        let stop = load_or_create("stop", &[1320.0, 880.0]);
        match (start, stop) {
            (Some(s), Some(t)) => Some(SoundIds { start: s, stop: t }),
            _ => None,
        }
    });

    let Some(ids) = ids else {
        return;
    };
    let id = if is_start { ids.start } else { ids.stop };
    unsafe { AudioServicesPlaySystemSound(id) };
}

/// 取出（必要时先生成）音效对应的系统音效 ID。
fn load_or_create(name: &str, freqs: &[f64]) -> Option<u32> {
    let dir = Config::app_dir().join("sounds");
    let path: PathBuf = dir.join(format!("{name}.wav"));

    if !path.exists() {
        let samples = synth(freqs);
        let bytes = wav_bytes(&samples, SAMPLE_RATE);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("音效目录创建失败 {}: {e}", dir.display());
            return None;
        }
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("音效文件写入失败 {}: {e}", path.display());
            return None;
        }
    }

    create_sound_id(&path)
}

/// 把文件注册成系统音效，返回 ID。
fn create_sound_id(path: &PathBuf) -> Option<u32> {
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let cf_str = CFStringCreateWithCString(
            std::ptr::null(),
            c_path.as_ptr(),
            CF_STRING_ENCODING_UTF8,
        );
        if cf_str.is_null() {
            return None;
        }
        let url = CFURLCreateWithFileSystemPath(
            std::ptr::null(),
            cf_str,
            CF_URL_POSIX_PATH_STYLE,
            false,
        );
        CFRelease(cf_str);
        if url.is_null() {
            return None;
        }
        let mut id: u32 = 0;
        let status = AudioServicesCreateSystemSoundID(url, &mut id);
        CFRelease(url);
        if status != 0 || id == 0 {
            eprintln!(
                "AudioServicesCreateSystemSoundID 失败（status={status}）: {}",
                path.display()
            );
            return None;
        }
        Some(id)
    }
}

/// 合成一段「每个音 90ms、带淡入淡出」的和弦（上行或下行由频率顺序决定）。
fn synth(freqs: &[f64]) -> Vec<i16> {
    const NOTE_MS: u32 = 90;
    const GAP_MS: u32 = 30;
    let mut out = Vec::new();
    for freq in freqs {
        let n = (SAMPLE_RATE as u64 * NOTE_MS as u64 / 1000) as usize;
        let total = n as f64;
        for k in 0..n {
            let t = k as f64 / SAMPLE_RATE as f64;
            // 简单的淡入淡出包络，避免爆音
            let env = (k as f64 / total).min(1.0 - k as f64 / total * 0.4).max(0.0);
            let s =
                (0.28 * env * (2.0 * std::f64::consts::PI * freq * t).sin()) * i16::MAX as f64;
            out.push(s as i16);
        }
        // 音之间留一点间隔
        let gap = (SAMPLE_RATE as u64 * GAP_MS as u64 / 1000) as usize;
        out.extend(std::iter::repeat(0i16).take(gap));
    }
    out
}

/// 手写一个 16-bit PCM 单声道 WAV。
fn wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk 大小
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    v.extend_from_slice(&2u16.to_le_bytes()); // block align
    v.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_valid() {
        let samples = synth(&[880.0]);
        let bytes = wav_bytes(&samples, SAMPLE_RATE);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[36..40], b"data");
        // 44 字节头 + 数据
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
    }

    #[test]
    fn synth_produces_expected_length() {
        // 两个音，每个 90ms + 30ms 间隔
        let samples = synth(&[880.0, 1320.0]);
        let expected = (SAMPLE_RATE as usize * 120 / 1000) * 2;
        assert_eq!(samples.len(), expected);
    }
}
