//! 系统剪贴板读写。
//!
//! 直接调用 macOS 自带的 `pbcopy` / `pbpaste`，避免引入 Objective-C 绑定，
//! 也避免对剪贴板里的非文本类型造成副作用（我们只关心文本）。

use std::io::Write;
use std::process::{Command, Stdio};

use vox_core::error::{Error, Result};

/// 读取剪贴板中的纯文本（**可能有损**：非 UTF-8 会被替换成 `�`）。
///
/// 只用来「给人看」。要**原样还原**剪贴板请用 [`read_clipboard_bytes`]。
pub fn read_clipboard() -> Result<String> {
    let bytes = read_clipboard_bytes()?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// 读取剪贴板原始字节。
///
/// **为什么要有这个**：剪贴板里可能不是合法 UTF-8（老程序写的 GBK、别的 App 的
/// 二进制数据…）。如果用 `read_clipboard()` 读再写回去，非法字节会被替换成 `�`，
/// **用户的剪贴板就被我们改成乱码了**。还原时必须用原始字节。
pub fn read_clipboard_bytes() -> Result<Vec<u8>> {
    let out = Command::new("pbpaste")
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

/// 写入原始字节到剪贴板（用于**原样还原**）。
pub fn write_clipboard_bytes(bytes: &[u8]) -> Result<()> {
    let mut child = Command::new("pbcopy")
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

/// 写入纯文本到剪贴板（UTF-8）。
pub fn write_clipboard(text: &str) -> Result<()> {
    write_clipboard_bytes(text.as_bytes())
}

/// 安全地**还原**剪贴板。
///
/// ## 为什么不能无脑写回
///
/// `pbcopy` 会把输入的字节按**当前 locale 的单字节字符集**解释，再编码成 UTF-8
/// 写进剪贴板。也就是说：**非 UTF-8 的内容（老程序的 GBK、别的 App 的二进制），
/// 原样写回去会变成「每个字节各变成一个 Latin-1 字符」的乱码**。
/// 实测：字节 `CA F2 E9 A7 A9` 写回后变成 `C2 A0 C3 9A C3 88 C3 9F` —— 全是乱码。
///
/// 所以：**只有原内容是合法 UTF-8 时才还原**；否则宁可不动剪贴板，
/// 也别把用户的内容改成乱码。
pub fn restore_clipboard_safely(bytes: &[u8]) -> bool {
    if std::str::from_utf8(bytes).is_err() {
        tracing::warn!("剪贴板原内容不是 UTF-8，跳过还原（写回会被 pbcopy 转成乱码）");
        return false;
    }
    write_clipboard_bytes(bytes).is_ok()
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

    /// 中文必须**逐字节**往返。
    ///
    /// （这两个断言必须在**同一个测试里**串行做：并行跑会互相覆盖剪贴板。）
    #[test]
    fn chinese_roundtrip_is_byte_exact() {
        let _guard = CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = read_clipboard_bytes().ok();
        let text = "明天多云转晴，气温 18 到 26 度。";

        write_clipboard(text).expect("写入剪贴板");
        let back = read_clipboard_bytes().expect("读回剪贴板");
        assert_eq!(back, text.as_bytes(), "中文往返必须逐字节一致");

        // 还原用户原来的剪贴板（用原始字节）
        if let Some(prev) = original {
            let _ = restore_clipboard_safely(&prev);
        }
    }

    /// **非 UTF-8 的内容不能被我们写成乱码**。
    ///
    /// `pbcopy` 会把这类字节按单字节字符集转成 UTF-8，所以还原时必须跳过。
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

        // 剪贴板里应该还是我们之前放的中文，没被这次调用动过
        if let Ok(cur) = read_clipboard_bytes() {
            assert!(
                std::str::from_utf8(&cur).is_ok(),
                "剪贴板不该被写成非 UTF-8 的乱码"
            );
        }

        if let Some(prev) = original {
            let _ = restore_clipboard_safely(&prev);
        }
    }
}
