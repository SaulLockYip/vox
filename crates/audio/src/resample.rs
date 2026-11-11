//! 重采样：低通 FIR（窗函数 sinc）+ 线性插值。
//!
//! 自带实现是为了避免额外依赖，并保证 48 kHz → 16 kHz 降采样时不会混叠。

/// 低通滤波 + 线性插值重采样。
pub fn resample(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    if out_rate == 0 || in_rate == 0 {
        return Vec::new();
    }

    // 截止频率取输出采样率的 45%，避免降采样混叠
    let cutoff = (out_rate as f64 * 0.45).min(in_rate as f64 * 0.45);
    let kernel = lowpass_kernel(cutoff / in_rate as f64, 63);

    let filtered = convolve(input, &kernel);

    let ratio = out_rate as f64 / in_rate as f64;
    let out_len = ((filtered.len() as f64) * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = filtered.get(idx).copied().unwrap_or(0.0);
        let b = filtered.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// 生成窗函数 sinc 低通核（cutoff 为归一化频率 0..0.5）。
fn lowpass_kernel(cutoff: f64, taps: usize) -> Vec<f32> {
    let n = taps as i64;
    let mid = (n / 2) as f64;
    let mut k = Vec::with_capacity(taps);
    for i in 0..taps {
        let x = i as f64 - mid;
        let sinc = if x.abs() < 1e-9 {
            2.0 * cutoff
        } else {
            (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
        };
        // Hann 窗
        let w = 0.5
            * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / (taps as f64 - 1.0)).cos());
        k.push((sinc * w) as f32);
    }
    // 归一化，避免增益变化
    let sum: f32 = k.iter().sum();
    if sum.abs() > 1e-6 {
        for v in k.iter_mut() {
            *v /= sum;
        }
    }
    k
}

/// 直接卷积（O(n·m)，录音时长有限，足够）。
fn convolve(input: &[f32], kernel: &[f32]) -> Vec<f32> {
    if kernel.is_empty() || input.is_empty() {
        return input.to_vec();
    }
    let half = kernel.len() / 2;
    let mut out = vec![0f32; input.len()];
    for i in 0..input.len() {
        let mut acc = 0f32;
        for (j, k) in kernel.iter().enumerate() {
            let idx = i as isize + j as isize - half as isize;
            if idx >= 0 && (idx as usize) < input.len() {
                acc += input[idx as usize] * k;
            }
        }
        out[i] = acc;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_is_identity() {
        let x = vec![0.1f32, 0.2, 0.3];
        assert_eq!(resample(&x, 16_000, 16_000), x);
    }

    #[test]
    fn empty_input() {
        assert!(resample(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn downsample_length_is_proportional() {
        let input = vec![0.0f32; 48_000]; // 1 秒 @48k
        let out = resample(&input, 48_000, 16_000);
        assert!((out.len() as i64 - 16_000).abs() <= 2, "len={}", out.len());
    }

    #[test]
    fn upsample_length_is_proportional() {
        let input = vec![0.0f32; 8_000];
        let out = resample(&input, 8_000, 16_000);
        assert!((out.len() as i64 - 16_000).abs() <= 2, "len={}", out.len());
    }

    #[test]
    fn constant_signal_stays_constant() {
        let input = vec![0.5f32; 4_800];
        let out = resample(&input, 48_000, 16_000);
        let mid = &out[100..out.len() - 100];
        let avg: f32 = mid.iter().sum::<f32>() / mid.len() as f32;
        assert!((avg - 0.5).abs() < 0.05, "avg={avg}");
    }

    #[test]
    fn silence_stays_silent() {
        let input = vec![0.0f32; 4_800];
        let out = resample(&input, 48_000, 16_000);
        assert!(out.iter().all(|v| v.abs() < 1e-6));
    }
}
