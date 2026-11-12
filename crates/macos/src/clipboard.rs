//! 系统剪贴板读写。
//!
//! ## 为什么不用 pbcopy / pbpaste 直接写
//!
//! macOS 的剪贴板（pasteboard）**同时保存多种 flavor**（utf8-plain-text、
//! utf16-plain-text、RTF…）。而：
//!
//! - `pbcopy` 只更新 **`public.utf8-plain-text`** 这一个 flavor；
//! - 目标 App 粘贴时用的是 **NSPasteboard 的 string**（utf16 flavor）。
//!
//! 于是只要剪贴板里**曾经**有一份用别的编码写进去的旧内容（老程序的 GBK、
//! 或者某次用错编码的写入），`pbcopy` 写什么，**目标 App 粘到的都还是那份旧内容**
//! —— 表现出来就是「明明写的是新内容，粘出来却是乱码」。
//!
//! 所以这里**写入和读取都走 NSPasteboard**（`osascript`）：
//! - 写入 = 清空所有 flavor + 统一写入 + 夺取 ownership ✓
//! - 读取 = 读到的**就是**目标 App 会拿到的那份 ✓（校验时才有意义）
//!
//! `pbcopy`/`pbpaste` 只作为**兜底**（osascript 不可用时）。

use std::io::Write;
use std::process::{Command, Stdio};

use vox_core::error::{Error, Result};

/// 让 `pbcopy` / `pbpaste` **一定按 UTF-8 处理**（兜底路径用）。
///
/// ## 两个坑（都实测过）
///
/// - ⚠️ **别写 `LC_ALL=en_US.UTF-8`**：这台机器没装这个 locale，会回退成 ASCII，
///   非 ASCII 字符直接变成 `?`（比乱码更糟）。
/// - ⚠️ **别乱设 `__CF_USER_TEXT_ENCODING`**：它要的是 `编码:脚本:变体` 三段格式，
///   写成 `0x08000100` 会被 CF 误解析 → 反而退回 Latin-1 → 乱码。
///   **正确做法是不碰它**，只把 locale 说成 UTF-8。
fn utf8_env(cmd: &mut Command) -> &mut Command {
    cmd.env("LC_CTYPE", "UTF-8").env("LC_ALL", "UTF-8")
}

/// 用 NSPasteboard 写剪贴板（**首选**）。
///
/// 文本通过 **argv** 传进去（不是拼进脚本），所以引号、反斜杠、换行都不用转义 ✓。
fn write_via_pasteboard(text: &str) -> Result<()> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("set the clipboard to item 1 of argv")
        .arg("-e")
        .arg("end run")
        .arg(text)
        .output()
        .map_err(|e| Error::Io(format!("调用 osascript 失败: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "osascript 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// 用 NSPasteboard 读剪贴板（**首选**）—— 读到的就是目标 App 会拿到的那份。
fn read_via_pasteboard() -> Result<String> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg("the clipboard as text")
        .output()
        .map_err(|e| Error::Io(format!("调用 osascript 失败: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "osascript 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    // AppleScript 的输出以换行结尾，去掉它
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    Ok(s)
}

/// 读取剪贴板中的纯文本。
///
/// 走 NSPasteboard（= 目标 App 实际会拿到的那份）；失败时退回 `pbpaste`。
pub fn read_clipboard() -> Result<String> {
    match read_via_pasteboard() {
        Ok(s) => Ok(s),
        Err(e) => {
            tracing::warn!("用 NSPasteboard 读剪贴板失败，改用 pbpaste: {e}");
            let bytes = read_clipboard_bytes()?;
            Ok(String::from_utf8_lossy(&bytes).to_string())
        }
    }
}

/// 读取剪贴板原始字节（`pbpaste`）。
///
/// 用来判断剪贴板里的内容**是不是合法 UTF-8** —— 不是的话**绝不能**原样写回去
/// （`pbcopy` 会把它按单字节字符集解释再编码 → 变成乱码）。
pub fn read_clipboard_bytes() -> Result<Vec<u8>> {
    let out = utf8_env(&mut Command::new("pbpaste"))
        .output()
        .map_err(|e| Error::Io(format!("调用 pbpaste 失败: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "pbpaste 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out.stdout)
}

/// 写入纯文本到剪贴板。
///
/// 走 NSPasteboard（清空所有 flavor + 统一写入）；失败时退回 `pbcopy`。
pub fn write_clipboard(text: &str) -> Result<()> {
    match write_via_pasteboard(text) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("用 NSPasteboard 写剪贴板失败，改用 pbcopy: {e}");
            write_clipboard_bytes(text.as_bytes())
        }
    }
}

/// 写入原始字节到剪贴板（`pbcopy`，用于**原样还原**）。
pub fn write_clipboard_bytes(bytes: &[u8]) -> Result<()> {
    let mut child = utf8_env(&mut Command::new("pbcopy"))
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Io(format!("启动 pbcopy 失败: {e}")))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| Error::Io("pbcopy 没有 stdin".into()))?;
        stdin
            .write_all(bytes)
            .map_err(|e| Error::Io(format!("写入 pbcopy 失败: {e}")))?;
    }
    let status = child
        .wait()
        .map_err(|e| Error::Io(format!("等待 pbcopy 结束失败: {e}")))?;
    if !status.success() {
        return Err(Error::Io(format!("pbcopy 退出码 {:?}", status.code())));
    }
    Ok(())
}

/// 安全地**还原**剪贴板。
///
/// ## 为什么不能无脑写回
///
/// `pbcopy` 会把输入的字节按**单字节字符集**解释，再编码成 UTF-8 写进剪贴板。
/// 也就是说：**非 UTF-8 的内容（老程序的 GBK、别的 App 的二进制），
/// 原样写回去会变成「每个字节各变成一个 Latin-1 字符」的乱码**。
/// 实测：字节 `CA F2 E9 A7 A9` 写回后变成 `C2 A0 C3 9A C3 88 C3 9F` —— 全是乱码。
///
/// 所以：**只有原内容是合法 UTF-8 时才还原**；否则宁可不动剪贴板，
/// 也别把用户的内容改成乱码。
pub fn restore_clipboard_safely(bytes: &[u8]) -> bool {
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!("剪贴板原内容不是 UTF-8，跳过还原（写回会被 pbcopy 转成乱码）");
            return false;
        }
    };
    write_clipboard(text).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 剪贴板是**全局唯一**资源，几个测试必须串行，否则会互相踩。
    static CLIPBOARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn read_clipboard_does_not_panic() {
        // 读写剪贴板会影响用户剪贴板，这里只验证读接口不 panic
        let _ = read_clipboard();
    }

    /// 中文必须**逐字节**往返（走 NSPasteboard，写入会清空所有 flavor）。
    #[test]
    fn chinese_roundtrip_is_byte_exact() {
        let _guard = CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = read_clipboard_bytes().ok();
        let text = "明天多云转晴，气温 18 到 26 度。";

        write_clipboard(text).expect("写入剪贴板");
        assert_eq!(read_clipboard().unwrap(), text, "中文往返必须一致");

        // 还原用户原来的剪贴板
        if let Some(prev) = original {
            let _ = restore_clipboard_safely(&prev);
        }
    }

    /// **非 UTF-8 的内容不能被我们写成乱码**。
    #[test]
    fn non_utf8_content_is_never_written_back_as_garbage() {
        let _guard = CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = read_clipboard_bytes().ok();
        let raw = vec![0xCAu8, 0xF2, 0xE9, 0xA7, 0xA9, 0x00, 0xFF];

        // 非 UTF-8 → 拒绝还原（宁可不动剪贴板）
        assert!(
            !restore_clipboard_safely(&raw),
            "非 UTF-8 的内容不该被写回（会变成乱码）"
        );

        if let Some(prev) = original {
            let _ = restore_clipboard_safely(&prev);
        }
    }

    /// 写入之后，**目标 App 读到的那份**必须就是新内容
    /// （这正是以前会出错的地方：pbcopy 只更新了一个 flavor，
    ///  App 粘到的还是旧的乱码）。
    #[test]
    fn write_updates_the_flavour_apps_actually_paste() {
        let _guard = CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = read_clipboard_bytes().ok();

        let text = "广州明天多云转晴，18 到 26 度，出门记得带伞。";
        write_clipboard(text).expect("写入剪贴板");

        // NSPasteboard 读到的 = App 粘到的
        assert_eq!(read_via_pasteboard().unwrap(), text);

        if let Some(prev) = original {
            let _ = restore_clipboard_safely(&prev);
        }
    }
}
