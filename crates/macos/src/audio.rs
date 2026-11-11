//! 录音时静音其它音频。
//!
//! 做法：拿到系统**默认输出设备**，把它的 `mute` 属性置 1；录完再还原成原值。
//! 这样你一边放音乐一边口述，音乐不会串进麦克风。

use std::ffi::c_void;

use vox_core::error::{Error, Result};

/// `kAudioObjectSystemObject`
const AUDIO_OBJECT_SYSTEM: u32 = 1;
/// `kAudioObjectPropertyScopeGlobal`（'glob'）
const SCOPE_GLOBAL: u32 = 0x676c_6f62;
/// `kAudioObjectPropertyScopeOutput`（'outp'）—— 设备级「输出流」属性要用它
const SCOPE_OUTPUT: u32 = 0x6f75_7470;
/// `kAudioObjectPropertyElementMain`
const MAIN_ELEMENT: u32 = 0;
/// `kAudioHardwarePropertyDefaultOutputDevice`（'dOut'）
const PROP_DEFAULT_OUTPUT_DEVICE: u32 = 0x644f_7574;
/// `kAudioHardwarePropertyDefaultInputDevice`（'dIn ' —— **末尾是空格**，不是 '!'）
const PROP_DEFAULT_INPUT_DEVICE: u32 = 0x6449_6e20;
/// `kAudioDevicePropertyMute`（'mute'）
const PROP_MUTE: u32 = 0x6d75_7465;
/// `kAudioDevicePropertyActive`（'actv'）
const PROP_ACTIVE: u32 = 0x6163_7476;
/// `kAudioDevicePropertyStreamConfiguration`（'slay'）
const PROP_STREAM_CONFIG: u32 = 0x736c_6179;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    m_selector: u32,
    m_scope: u32,
    m_element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyData(
        in_object_id: u32,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const c_void,
        io_data_size: *mut u32,
        out_data: *mut c_void,
    ) -> i32;

    fn AudioObjectSetPropertyData(
        in_object_id: u32,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const c_void,
        io_data_size: u32,
        in_data: *const c_void,
    ) -> i32;
}

/// 读一个 u32 属性。失败返回 None。
fn get_u32(object: u32, addr: &AudioObjectPropertyAddress) -> Option<u32> {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut value as *mut u32 as *mut c_void,
        )
    };
    if status != 0 {
        return None;
    }
    Some(value)
}

/// 写一个 u32 属性。
fn set_u32(object: u32, addr: &AudioObjectPropertyAddress, value: u32) -> bool {
    let size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            addr,
            0,
            std::ptr::null(),
            size,
            &value as *const u32 as *const c_void,
        )
    };
    status == 0
}

/// 系统默认输出设备 ID。
fn default_output_device() -> Option<u32> {
    let addr = AudioObjectPropertyAddress {
        m_selector: PROP_DEFAULT_OUTPUT_DEVICE,
        m_scope: SCOPE_GLOBAL,
        m_element: MAIN_ELEMENT,
    };
    get_u32(AUDIO_OBJECT_SYSTEM, &addr)
}

/// 静音前的状态快照。`Drop` 时自动还原。
///
/// ```ignore
/// let guard = MuteGuard::mute().ok();
/// // …录音中…
/// drop(guard); // 自动还原
/// ```
#[derive(Debug, Clone)]
pub struct MuteGuard {
    device: u32,
    was_muted: bool,
}

impl MuteGuard {
    /// 静音默认输出设备；返回快照（Drop 时还原）。
    pub fn mute() -> Result<MuteGuard> {
        let device = default_output_device().ok_or_else(|| {
            Error::Audio("找不到系统默认输出设备（无法静音其它音频）".into())
        })?;
        let mute_addr = AudioObjectPropertyAddress {
            m_selector: PROP_MUTE,
            m_scope: SCOPE_GLOBAL,
            m_element: MAIN_ELEMENT,
        };
        let was_muted = get_u32(device, &mute_addr).map(|v| v != 0).unwrap_or(false);
        if !set_u32(device, &mute_addr, 1) {
            return Err(Error::Audio("设置输出设备静音失败".into()));
        }
        Ok(MuteGuard { device, was_muted })
    }

    /// 立刻还原（Drop 也会调它，重复调用无副作用）。
    pub fn restore(&self) {
        if self.was_muted {
            return; // 本来就是静音的，别去动用户的设置
        }
        let mute_addr = AudioObjectPropertyAddress {
            m_selector: PROP_MUTE,
            m_scope: SCOPE_GLOBAL,
            m_element: MAIN_ELEMENT,
        };
        set_u32(self.device, &mute_addr, 0);
    }
}

impl Drop for MuteGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// 系统默认输入设备 ID。
fn default_input_device() -> Option<u32> {
    let addr = AudioObjectPropertyAddress {
        m_selector: PROP_DEFAULT_INPUT_DEVICE,
        m_scope: SCOPE_GLOBAL,
        m_element: MAIN_ELEMENT,
    };
    get_u32(AUDIO_OBJECT_SYSTEM, &addr)
}

/// 默认输出设备的声道数。
///
/// **2 = A2DP**（蓝牙立体声，音质正常）；**1 = HFP**（蓝牙「通话模式」，
/// 单声道 16k，音质差）。蓝牙耳机被打开麦克风时就会变成 1。
fn output_channel_count(device: u32) -> Option<u32> {
    let addr = AudioObjectPropertyAddress {
        m_selector: PROP_STREAM_CONFIG,
        m_scope: SCOPE_OUTPUT,
        m_element: MAIN_ELEMENT,
    };
    // AudioBufferList: mNumberBuffers(4) + 填充(4) + AudioBuffer{ mNumberChannels(4), mData(8) }
    let mut buf = [0u8; 32];
    let mut size = buf.len() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            buf.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 || size < 12 {
        return None;
    }
    let n_buffers = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if n_buffers == 0 {
        return None;
    }
    Some(u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]]))
}

/// 默认输出设备现在是不是卡在 **HFP**（单声道）？
///
/// - `Some(true)`  → 单声道：蓝牙「通话模式」，音质差（录音期间本来就该这样）
/// - `Some(false)` → 立体声：A2DP，音质正常
/// - `None`        → 读不到（没有输出设备）
pub fn output_is_mono_hfp() -> Option<bool> {
    let output = default_output_device()?;
    output_channel_count(output).map(|c| c == 1)
}

/// 录音结束后，如果蓝牙耳机还卡在 HFP（单声道 16k），把它推回 A2DP。
///
/// ## 为什么需要
///
/// 蓝牙耳机一旦被打开麦克风，链路会从 **A2DP**（立体声 44.1/48 kHz）切到
/// **HFP**（单声道 16 kHz）—— **录音期间音质变差是正常的**。
/// 麦克风释放后 macOS 通常会自己切回 A2DP，但**有时会卡住不切**，
/// 表现就是「都录完了，耳机音质还是差的」。
///
/// 卡住的根因几乎都是**还有别的进程占着麦克风**（实测：一个没退干净的
/// `vox hotkey` 调试进程占了两天，耳机一直是单声道）。本项目的三道防线：
/// - `vox_core::instance::HOTKEY_LOCK` —— 全局热键全局只能有一个监听者；
/// - `RecordingSession` 的 `Drop` —— 会话被丢弃时一定让采集线程停下，
///   不会留下「永远开着麦克风」的孤儿录音器；
/// - `spawn_recording` 开始新会话前先把上一个会话停掉。
///
/// 这个函数是**兜底**：只有当输出设备确实还是单声道（HFP）时才动手，
/// 正常状态下一律不碰（实测对已正常的设备是无害空操作）。
pub fn restore_bluetooth_profile() -> bool {
    // ① 只有卡在单声道（HFP）时才需要救
    let Some(output) = default_output_device() else {
        return false;
    };
    if output_channel_count(output) != Some(1) {
        return false; // 已经正常（或读不到）→ 不动
    }

    let Some(input) = default_input_device() else {
        return false;
    };

    // ② 停用 → 启用：让 HAL 真正释放输入设备，蓝牙栈于是重新协商
    let active_addr = AudioObjectPropertyAddress {
        m_selector: PROP_ACTIVE,
        m_scope: SCOPE_GLOBAL,
        m_element: MAIN_ELEMENT,
    };
    let deactivated = set_u32(input, &active_addr, 0);
    if deactivated {
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
    let reactivated = set_u32(input, &active_addr, 1);

    // ③ 把默认输入设备重新设置一遍（值不变）—— 让 coreaudiod 重新激活它
    let in_addr = AudioObjectPropertyAddress {
        m_selector: PROP_DEFAULT_INPUT_DEVICE,
        m_scope: SCOPE_GLOBAL,
        m_element: MAIN_ELEMENT,
    };
    let size = std::mem::size_of::<u32>() as u32;
    let reassigned = unsafe {
        AudioObjectSetPropertyData(
            AUDIO_OBJECT_SYSTEM,
            &in_addr,
            0,
            std::ptr::null(),
            size,
            &input as *const u32 as *const c_void,
        ) == 0
    };

    let acted = (deactivated && reactivated) || reassigned;
    if acted {
        tracing::info!("蓝牙输出还卡在单声道(HFP)，已重新激活输入设备以切回 A2DP");
    }
    acted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_device_is_readable() {
        // 本机应该能读到默认输出设备；读不到也不该 panic
        let _ = default_output_device();
    }

    #[test]
    fn mute_and_restore_roundtrip() {
        // 静音 → 还原后状态应回到原样
        let Some(guard) = MuteGuard::mute().ok() else {
            return; // 没有输出设备就跳过
        };
        let device = guard.device;
        let addr = AudioObjectPropertyAddress {
            m_selector: PROP_MUTE,
            m_scope: SCOPE_GLOBAL,
            m_element: MAIN_ELEMENT,
        };
        assert_eq!(get_u32(device, &addr), Some(1), "应已静音");
        drop(guard);
        assert_ne!(get_u32(device, &addr), Some(1), "应已还原");
    }

    /// 默认输入/输出设备都要读得到（FourCC 写错一个字节就读不到了）。
    #[test]
    fn default_devices_are_readable() {
        // 读不到就跳过（没有音频设备的环境）
        if default_output_device().is_none() {
            return;
        }
        assert!(default_input_device().is_some(), "应能读到默认输入设备");
    }

    /// 输出设备的声道数读得到（A2DP=2 / HFP=1）。
    #[test]
    fn output_channel_count_is_readable() {
        let Some(device) = default_output_device() else {
            return;
        };
        let ch = output_channel_count(device);
        assert!(ch.is_some(), "应能读到输出声道数");
        assert!(ch.unwrap() >= 1);
    }

    /// 恢复蓝牙 profile 的动作必须是「无害」的：设备已经正常时不动它。
    #[test]
    fn restore_is_noop_when_device_is_fine() {
        // 正常设备（立体声）时不该有任何动作
        if output_is_mono_hfp() == Some(false) {
            assert!(!restore_bluetooth_profile(), "设备正常时不应动手");
        }
    }
}
