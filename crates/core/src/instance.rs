//! 跨进程互斥锁（`flock`）。
//!
//! ## 为什么需要
//!
//! 全局热键和麦克风都是**系统级独占资源**：同一时刻只能有一个进程在听、
//! 只有一个进程在录。曾经出过这样的事故：
//!
//! - 桌面上跑着 `Vox.app`
//! - 终端里还挂着一个**遗留的调试进程** `vox hotkey`（跑了两天多）
//!
//! 两边同时收到热键、同时开麦（于是产生两个时长几乎一样的录音文件）、
//! 各自跑完 ASR/LLM、**各自把结果插进光标** —— 用户看到同一句话被插了两遍。
//!
//! 所以这里用文件锁把「全局热键」「应用实例」这类资源锁起来：
//! 谁抢到锁谁干活，抢不到的直接退出并给出明确提示。
//!
//! 用 `flock` 而不是 PID 文件：进程一死锁自动释放，不会留下「假死」的死锁。

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use crate::error::{Error, Result};

extern "C" {
    /// BSD `flock`（macOS 在 libSystem 里，Linux 也有同名函数）。
    fn flock(fd: i32, operation: i32) -> i32;
}
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

/// 抢一把**命名锁**。
///
/// - `name` 是锁的名字（例如 `"hotkey-listener"`），不同名字互不影响。
/// - 锁文件放在应用配置目录里（`<app_dir>/<name>.lock`）。
/// - 拿不到锁说明**别的进程正持有**它 → 返回错误，调用方应当退出。
///
/// 返回的 `File` 必须**一直持有**（比如存进一个 static 里），锁才会一直有效；
/// 进程退出时自动释放。
pub fn acquire(name: &str) -> Result<File> {
    let path = lock_path(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if rc != 0 {
        return Err(Error::Other(format!(
            "锁 `{name}` 已被其它进程占用（另一个 Vox 正在运行？）"
        )));
    }
    Ok(file)
}

/// 锁文件路径。
pub fn lock_path(name: &str) -> PathBuf {
    crate::config::Config::app_dir().join(format!("{name}.lock"))
}

/// 全局热键监听器的锁名。
///
/// **app 与 CLI 的 `vox hotkey` 共用这一把锁** —— 保证同一时刻只有一个进程在听热键。
pub const HOTKEY_LOCK: &str = "hotkey-listener";

/// 应用实例的锁名（保证只跑一个 `Vox.app`）。
pub const INSTANCE_LOCK: &str = "instance";

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一把锁不能被抢两次。
    #[test]
    fn lock_is_exclusive() {
        let name = format!("test-lock-{}", std::process::id());
        let first = acquire(&name);
        assert!(first.is_ok(), "第一次应该拿得到");
        let second = acquire(&name);
        assert!(second.is_err(), "第二次必须失败");
        // 放掉第一把之后就能拿到
        drop(first);
        let third = acquire(&name);
        assert!(third.is_ok(), "释放后应该能拿到");
        let _ = std::fs::remove_file(lock_path(&name));
    }

    /// 不同名字互不影响。
    #[test]
    fn different_locks_are_independent() {
        let a = format!("test-a-{}", std::process::id());
        let b = format!("test-b-{}", std::process::id());
        let _ga = acquire(&a);
        let gb = acquire(&b);
        assert!(gb.is_ok(), "不同的锁名应该互不影响");
        let _ = std::fs::remove_file(lock_path(&a));
        let _ = std::fs::remove_file(lock_path(&b));
    }
}
