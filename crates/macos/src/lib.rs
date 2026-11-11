//! macOS 平台集成：剪贴板、文本注入、活跃应用、全局热键、辅助功能读取、开机自启。

pub mod autostart;
pub mod audio;
pub mod ax;
pub mod clipboard;
pub mod hotkey;
pub mod inject;
pub mod sound;

pub use autostart::{is_launch_at_login, set_launch_at_login};
pub use ax::{frontmost_app_pid, selected_text};
pub use clipboard::{read_clipboard, write_clipboard};
pub use hotkey::{HotkeyEvent, HotkeyListener, KeyCombo};
pub use inject::{frontmost_app_name, insert_text, InsertMethod};
