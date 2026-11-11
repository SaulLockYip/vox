//! 全局热键监听（自实现 macOS CGEventTap）。
//!
//! ## 为什么不用 rdev
//!
//! rdev 0.5 在 macOS 上会把**每一个按键**都转成字符，转换过程要调用
//! `TISCopyCurrentKeyboardInputSource` / `TISGetInputSourceProperty`。
//! TextInputSource 系列 API 要求在**主线程**调用，而 rdev 的 event tap 回调跑在
//! 它自己的 CFRunLoop 线程上，于是命中 `dispatch_assert_queue_fail` → SIGTRAP →
//! 进程当场崩溃（崩溃报告 `~/Library/Logs/DiagnosticReports/Vox-*.ips`，崩溃帧是
//! `rdev::macos::keyboard::string_from_code`）。也就是说**按任意一个键都会崩**。
//!
//! 这里改为直接读 CGEvent 的虚拟键码（virtual keycode），自己维护修饰键状态，
//! 完全不碰 TextInputSource，也就不会崩。
//!
//! ## 匹配语义（集合精确匹配）
//!
//! - 按下的键集合**恰好等于**某个绑定 → 该绑定生效
//! - 集合里**再加**一个键且恰好等于另一个绑定 → 切换到新绑定
//! - 松开键：当前模式**保持不变**（避免「先松开修饰键导致模式跳回」的抖动）
//! - 全部松开 → 会话结束
//!
//! 需要 macOS「辅助功能」权限。

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use vox_core::error::{Error, Result};

// ---------------------------------------------------------------------------
// 虚拟键码
// ---------------------------------------------------------------------------

/// 一个物理按键（对应 macOS 的 `kVK_*` 虚拟键码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    LeftCtrl,
    RightCtrl,
    LeftShift,
    RightShift,
    LeftAlt,
    RightAlt,
    LeftCmd,
    RightCmd,
    /// Fn / 地球仪键
    Fn,
    /// 菜单键
    ContextMenu,
    Space,
    Return,
    Tab,
    Escape,
    Backspace,
    ForwardDelete,
    LeftArrow,
    RightArrow,
    UpArrow,
    DownArrow,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
}

impl Key {
    /// macOS 虚拟键码。
    pub const fn keycode(self) -> u16 {
        match self {
            Key::LeftCtrl => 59,
            Key::RightCtrl => 62,
            Key::LeftShift => 56,
            Key::RightShift => 60,
            Key::LeftAlt => 58,
            Key::RightAlt => 61,
            Key::LeftCmd => 55,
            Key::RightCmd => 54,
            Key::Fn => 63,
            Key::ContextMenu => 110,
            Key::Space => 49,
            Key::Return => 36,
            Key::Tab => 48,
            Key::Escape => 53,
            Key::Backspace => 51,
            Key::ForwardDelete => 117,
            Key::LeftArrow => 123,
            Key::RightArrow => 124,
            Key::UpArrow => 126,
            Key::DownArrow => 125,
            Key::A => 0,
            Key::B => 11,
            Key::C => 8,
            Key::D => 2,
            Key::E => 14,
            Key::F => 3,
            Key::G => 5,
            Key::H => 4,
            Key::I => 34,
            Key::J => 38,
            Key::K => 40,
            Key::L => 37,
            Key::M => 46,
            Key::N => 45,
            Key::O => 31,
            Key::P => 35,
            Key::Q => 12,
            Key::R => 15,
            Key::S => 1,
            Key::T => 17,
            Key::U => 32,
            Key::V => 9,
            Key::W => 13,
            Key::X => 7,
            Key::Y => 16,
            Key::Z => 6,
            Key::Num0 => 29,
            Key::Num1 => 18,
            Key::Num2 => 19,
            Key::Num3 => 20,
            Key::Num4 => 21,
            Key::Num5 => 23,
            Key::Num6 => 22,
            Key::Num7 => 26,
            Key::Num8 => 28,
            Key::Num9 => 25,
            Key::F1 => 122,
            Key::F2 => 120,
            Key::F3 => 99,
            Key::F4 => 118,
            Key::F5 => 96,
            Key::F6 => 97,
            Key::F7 => 98,
            Key::F8 => 100,
            Key::F9 => 101,
            Key::F10 => 109,
            Key::F11 => 103,
            Key::F12 => 111,
            Key::F13 => 105,
            Key::F14 => 107,
            Key::F15 => 113,
            Key::F16 => 106,
            Key::F17 => 64,
            Key::F18 => 79,
            Key::F19 => 80,
            Key::F20 => 90,
        }
    }

    /// 由虚拟键码反查按键；不是我们关心的键返回 None。
    pub fn from_keycode(code: u16) -> Option<Key> {
        const TABLE: &[(u16, Key)] = &[
            (59, Key::LeftCtrl),
            (62, Key::RightCtrl),
            (56, Key::LeftShift),
            (60, Key::RightShift),
            (58, Key::LeftAlt),
            (61, Key::RightAlt),
            (55, Key::LeftCmd),
            (54, Key::RightCmd),
            (63, Key::Fn),
            (110, Key::ContextMenu),
            (49, Key::Space),
            (36, Key::Return),
            (48, Key::Tab),
            (53, Key::Escape),
            (51, Key::Backspace),
            (117, Key::ForwardDelete),
            (123, Key::LeftArrow),
            (124, Key::RightArrow),
            (126, Key::UpArrow),
            (125, Key::DownArrow),
            (0, Key::A),
            (11, Key::B),
            (8, Key::C),
            (2, Key::D),
            (14, Key::E),
            (3, Key::F),
            (5, Key::G),
            (4, Key::H),
            (34, Key::I),
            (38, Key::J),
            (40, Key::K),
            (37, Key::L),
            (46, Key::M),
            (45, Key::N),
            (31, Key::O),
            (35, Key::P),
            (12, Key::Q),
            (15, Key::R),
            (1, Key::S),
            (17, Key::T),
            (32, Key::U),
            (9, Key::V),
            (13, Key::W),
            (7, Key::X),
            (16, Key::Y),
            (6, Key::Z),
            (29, Key::Num0),
            (18, Key::Num1),
            (19, Key::Num2),
            (20, Key::Num3),
            (21, Key::Num4),
            (23, Key::Num5),
            (22, Key::Num6),
            (26, Key::Num7),
            (28, Key::Num8),
            (25, Key::Num9),
            (122, Key::F1),
            (120, Key::F2),
            (99, Key::F3),
            (118, Key::F4),
            (96, Key::F5),
            (97, Key::F6),
            (98, Key::F7),
            (100, Key::F8),
            (101, Key::F9),
            (109, Key::F10),
            (103, Key::F11),
            (111, Key::F12),
            (105, Key::F13),
            (107, Key::F14),
            (113, Key::F15),
            (106, Key::F16),
            (64, Key::F17),
            (79, Key::F18),
            (80, Key::F19),
            (90, Key::F20),
        ];
        TABLE.iter().find(|(c, _)| *c == code).map(|(_, k)| *k)
    }

    /// 是否是「按住不放」类的修饰键（macOS 用 FlagsChanged 事件上报）。
    pub const fn is_modifier(self) -> bool {
        matches!(
            self,
            Key::LeftCtrl
                | Key::RightCtrl
                | Key::LeftShift
                | Key::RightShift
                | Key::LeftAlt
                | Key::RightAlt
                | Key::LeftCmd
                | Key::RightCmd
                | Key::Fn
                | Key::ContextMenu
        )
    }

    /// 该键对应的**聚合**修饰键标志位（`NX_*`）。
    ///
    /// macOS 的 `FlagsChanged` 事件只告诉我们「这一类修饰键整体按下还是抬起」，
    /// 不区分左右。所以判断某个具体键是按下还是松开，要靠这个标志位：
    /// - 标志位**开** → 该类修饰键处于按下态
    /// - 标志位**关** → 该类修饰键已全部松开
    ///
    /// Fn 键没有对应的聚合标志（flags 里不体现），返回 None。
    pub const fn modifier_flag(self) -> Option<u64> {
        match self {
            Key::LeftShift | Key::RightShift => Some(FLAG_SHIFT),
            Key::LeftCtrl | Key::RightCtrl => Some(FLAG_CONTROL),
            Key::LeftAlt | Key::RightAlt => Some(FLAG_ALTERNATE),
            Key::LeftCmd | Key::RightCmd => Some(FLAG_COMMAND),
            _ => None,
        }
    }
}

/// 一个按键组合，如 `RightCtrl+RightShift`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCombo {
    pub keys: Vec<Key>,
}

impl KeyCombo {
    /// 解析 `"RightCtrl+RightShift"` 这类写法。
    pub fn parse(spec: &str) -> Result<KeyCombo> {
        let mut keys = Vec::new();
        for part in spec.split('+') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            keys.push(parse_key(part)?);
        }
        if keys.is_empty() {
            return Err(Error::Config(format!("无效的快捷键: {spec}")));
        }
        // 同一个键写两遍没有意义，直接报错比静默去重更好排查
        let mut seen = std::collections::HashSet::new();
        for k in &keys {
            if !seen.insert(*k) {
                return Err(Error::Config(format!("快捷键里有重复按键: {spec}")));
            }
        }
        Ok(KeyCombo { keys })
    }

    /// 精确集合匹配：当前按下的键集合必须**恰好等于**绑定集合。
    fn matches_exactly(&self, pressed: &HashSet<Key>) -> bool {
        pressed.len() == self.keys.len() && self.keys.iter().all(|k| pressed.contains(k))
    }
}

/// 把配置里的按键名映射到 [`Key`]。
pub fn parse_key(name: &str) -> Result<Key> {
    let key = match name {
        "RightCtrl" | "RightControl" | "RCtrl" => Key::RightCtrl,
        "LeftCtrl" | "LeftControl" | "LCtrl" => Key::LeftCtrl,
        "RightShift" | "RShift" => Key::RightShift,
        "LeftShift" | "LShift" => Key::LeftShift,
        "RightAlt" | "RightOption" | "ROption" => Key::RightAlt,
        "LeftAlt" | "LeftOption" | "LOption" | "Alt" | "Option" => Key::LeftAlt,
        "RightCmd" | "RightMeta" | "RCmd" => Key::RightCmd,
        "LeftCmd" | "LeftMeta" | "LCmd" | "Meta" | "Cmd" | "Command" => Key::LeftCmd,
        "Space" => Key::Space,
        "Enter" | "Return" => Key::Return,
        "Tab" => Key::Tab,
        "Escape" | "Esc" => Key::Escape,
        "Backspace" => Key::Backspace,
        "ForwardDelete" => Key::ForwardDelete,
        "LeftArrow" | "Left" => Key::LeftArrow,
        "RightArrow" | "Right" => Key::RightArrow,
        "UpArrow" | "Up" => Key::UpArrow,
        "DownArrow" | "Down" => Key::DownArrow,
        "Fn" | "Function" | "Globe" => Key::Fn,
        "ContextMenu" | "Menu" => Key::ContextMenu,
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => match c.to_ascii_lowercase() {
                    'a' => Key::A,
                    'b' => Key::B,
                    'c' => Key::C,
                    'd' => Key::D,
                    'e' => Key::E,
                    'f' => Key::F,
                    'g' => Key::G,
                    'h' => Key::H,
                    'i' => Key::I,
                    'j' => Key::J,
                    'k' => Key::K,
                    'l' => Key::L,
                    'm' => Key::M,
                    'n' => Key::N,
                    'o' => Key::O,
                    'p' => Key::P,
                    'q' => Key::Q,
                    'r' => Key::R,
                    's' => Key::S,
                    't' => Key::T,
                    'u' => Key::U,
                    'v' => Key::V,
                    'w' => Key::W,
                    'x' => Key::X,
                    'y' => Key::Y,
                    'z' => Key::Z,
                    '0' => Key::Num0,
                    '1' => Key::Num1,
                    '2' => Key::Num2,
                    '3' => Key::Num3,
                    '4' => Key::Num4,
                    '5' => Key::Num5,
                    '6' => Key::Num6,
                    '7' => Key::Num7,
                    '8' => Key::Num8,
                    '9' => Key::Num9,
                    _ => return Err(Error::Config(format!("未知按键: {other}"))),
                },
                _ => return Err(Error::Config(format!("未知按键: {other}"))),
            }
        }
    };
    Ok(key)
}

/// 热键事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// 某个绑定开始生效。
    ///
    /// `master_pressed` = 这次是**主控键（听写热键）本身被按下**。
    /// 主控键被按下时**一定**会发这个事件（哪怕按下的集合没变），
    /// 因为主控键是所有模式统一的「结束键」。
    ComboDown {
        feature: String,
        master_pressed: bool,
    },
    /// 全部按键松开，会话结束。
    ComboUp { feature: String },
}

// ---------------------------------------------------------------------------
// CGEventTap
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopMode = *const c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

/// `kCGSessionEventTap`
///
/// 用 session 级而不是 HID 级：HID 级的 tap 位于系统处理按键之前，
/// 收不到 `CGEventPost` 注入的合成事件（也就没法自动验证），
/// 而 session 级是全局热键的常规选择，真实按键一样能收到。
const TAP_SESSION: u32 = 1;
/// `kCGHeadInsertEventTap`
const TAP_HEAD_INSERT: u32 = 0;
/// `kCGEventTapOptionListenOnly`
const TAP_LISTEN_ONLY: u32 = 1;
/// `kCGEventKeyDown`
const EVENT_KEY_DOWN: u32 = 10;
/// `kCGEventKeyUp`
const EVENT_KEY_UP: u32 = 11;
/// `kCGEventFlagsChanged`
const EVENT_FLAGS_CHANGED: u32 = 12;
/// `kCGEventTapDisabledByTimeout`
const EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xffff_fffe;
/// `kCGEventTapDisabledByUserInput`
const EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xffff_ffff;
/// `kCGKeyboardEventKeycode`
const FIELD_KEYCODE: u32 = 9;

/// `NX_SHIFTMASK` — Shift 聚合标志
const FLAG_SHIFT: u64 = 1 << 17;
/// `NX_CONTROLMASK` — Control 聚合标志
const FLAG_CONTROL: u64 = 1 << 18;
/// `NX_ALTERNATEMASK` — Option 聚合标志
const FLAG_ALTERNATE: u64 = 1 << 19;
/// `NX_COMMANDMASK` — Command 聚合标志
const FLAG_COMMAND: u64 = 1 << 20;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: CFTypeRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(
        rl: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: CFRunLoopMode,
    );
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    fn CFRelease(cf: CFTypeRef);
    static kCFRunLoopCommonModes: CFRunLoopMode;
}

/// 监听器内部状态（由 event tap 回调访问）。
struct TapState {
    bindings: Vec<(String, KeyCombo)>,
    /// 主控绑定（听写热键）—— 它既是「听写」的开始键，也是所有模式统一的**结束键**。
    master: String,
    master_keys: Vec<Key>,
    pressed: Mutex<HashSet<Key>>,
    /// 当前生效的绑定。
    active: Mutex<Option<String>>,
    tx: Sender<HotkeyEvent>,
    /// tap 端口，用于被系统禁用后重新启用。
    tap: AtomicPtr<c_void>,
}

impl TapState {
    /// 某个键被按下（普通键）。
    fn on_press(&self, code: u16) {
        let Some(key) = Key::from_keycode(code) else {
            return;
        };
        let Ok(mut pressed) = self.pressed.lock() else {
            return;
        };
        // 已经有这个键了（按键重复 / 系统补发的 KeyDown）→ 状态没变
        if !pressed.insert(key) {
            return;
        }
        // 主控键被按下 → **一定**发事件（哪怕集合没变），因为它是统一的结束键
        let master_pressed = self.master_keys.contains(&key);
        self.emit_change(&pressed, master_pressed);
    }

    /// 按键松开。
    fn on_release(&self, code: u16) {
        let Some(key) = Key::from_keycode(code) else {
            return;
        };
        let Ok(mut pressed) = self.pressed.lock() else {
            return;
        };
        if !pressed.remove(&key) {
            return;
        }
        if pressed.is_empty() {
            // 全部松开 → 会话结束
            self.end_session();
            return;
        }
        // **松开键不重新匹配**：模式保持锁存。
        // 否则「松开 Space → 集合恰好等于主控绑定」会把 Ask 抖回听写，
        // 而且在主控键结束会话之后还会凭空开一段新录音。
        // 主控键结束会话靠的是它**被按下**时发出的事件，不靠集合匹配。
    }

    /// 修饰键状态变化（Shift/Ctrl/Option/Cmd/Fn）。
    ///
    /// 方向判定**不用**「集合里有没有它」（那有歧义），而是用事件 flags 里的
    /// **聚合标志位**：该类修饰键整体是按下还是全部松开。
    ///
    /// 这样即使系统同时补发 `KeyUp`（先 KeyUp 结束会话、紧接着 FlagsChanged 又被
    /// 误判成「按下」而重新开始录音且再也停不下来）也不会出错：
    /// 补发的 FlagsChanged 里聚合标志是**关**的，直接忽略。
    fn on_flags_changed(&self, code: u16, flags: u64) {
        let Some(key) = Key::from_keycode(code) else {
            return;
        };
        if !key.is_modifier() {
            return;
        }
        let Ok(mut pressed) = self.pressed.lock() else {
            return;
        };

        match key.modifier_flag() {
            Some(mask) => {
                let agg_down = flags & mask != 0;
                let in_set = pressed.contains(&key);
                if agg_down && !in_set {
                    // 明确按下
                    pressed.insert(key);
                    let master_pressed = self.master_keys.contains(&key);
                    self.emit_change(&pressed, master_pressed);
                } else if !agg_down && in_set {
                    // 明确松开
                    pressed.remove(&key);
                    if pressed.is_empty() {
                        self.end_session();
                    }
                    // 松开不重新匹配（模式保持锁存），见 on_release 的说明
                }
                // agg_down && in_set  → 同类型的另一侧键被按下，本键状态不变
                // !agg_down && !in_set → 我们没见过它的按下事件（启动时就按着），忽略
            }
            None => {
                // 没有可靠聚合标志的修饰键（Fn）：只能靠集合开关判断方向
                if pressed.remove(&key) {
                    if pressed.is_empty() {
                        self.end_session();
                    }
                    // 松开不重新匹配
                } else {
                    pressed.insert(key);
                    let master_pressed = self.master_keys.contains(&key);
                    self.emit_change(&pressed, master_pressed);
                }
            }
        }
    }

    /// 当前按下的集合匹配到哪个绑定。
    fn match_binding(&self, pressed: &HashSet<Key>) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, combo)| combo.matches_exactly(pressed))
            .map(|(feature, _)| feature.clone())
    }

    /// 状态变化时发事件。
    ///
    /// - `master_pressed` 为 true（主控键被按下）：**一定**发，哪怕集合没变
    /// - 否则只有「匹配到的绑定变了」才发
    /// - 集合**不匹配任何绑定**时保持当前模式不动（不切回主控绑定）
    fn emit_change(&self, pressed: &HashSet<Key>, master_pressed: bool) {
        let matched = self.match_binding(pressed);

        let Ok(mut active) = self.active.lock() else {
            return;
        };
        let changed = matched.is_some() && matched.as_deref() != active.as_deref();
        if let Some(feature) = &matched {
            *active = Some(feature.clone());
        }
        if !master_pressed && !changed {
            return;
        }
        let feature = matched.unwrap_or_else(|| self.master.clone());
        let _ = self.tx.send(HotkeyEvent::ComboDown {
            feature,
            master_pressed,
        });
    }

    fn end_session(&self) {
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        if let Some(feature) = active.take() {
            let _ = self.tx.send(HotkeyEvent::ComboUp { feature });
        }
    }
}

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if user_info.is_null() {
        return event;
    }
    let state = &*(user_info as *const TapState);

    // tap 被系统禁用（回调太慢 / 用户输入）→ 重新启用，否则热键永久失灵
    if event_type == EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        let tap = state.tap.load(Ordering::Relaxed);
        if !tap.is_null() {
            CGEventTapEnable(tap as CFMachPortRef, true);
        }
        return event;
    }

    let code = CGEventGetIntegerValueField(event, FIELD_KEYCODE) as u16;
    match event_type {
        EVENT_KEY_DOWN => state.on_press(code),
        EVENT_KEY_UP => state.on_release(code),
        EVENT_FLAGS_CHANGED => state.on_flags_changed(code, CGEventGetFlags(event)),
        _ => {}
    }
    event
}

/// 全局热键监听器。
pub struct HotkeyListener {
    rx: Receiver<HotkeyEvent>,
}

impl HotkeyListener {
    /// 启动监听。`bindings` 为 `(功能名, 快捷键)` 列表。
    ///
    /// 立即返回；真正的 event tap 在后台线程里跑。没有「辅助功能」权限时返回错误。
    pub fn start(bindings: Vec<(String, KeyCombo)>) -> Result<HotkeyListener> {
        let (tx, rx) = channel::<HotkeyEvent>();
        let (handshake_tx, handshake_rx) = std::sync::mpsc::sync_channel::<Result<()>>(0);

        // 主控绑定 = 听写热键：既是「听写」的开始键，也是所有模式统一的结束键
        let master = vox_core::mode::MASTER_FEATURE.to_string();
        let master_keys = bindings
            .iter()
            .find(|(f, _)| *f == master)
            .map(|(_, c)| c.keys.clone())
            .unwrap_or_default();

        let state = Arc::new(TapState {
            bindings,
            master,
            master_keys,
            pressed: Mutex::new(HashSet::new()),
            active: Mutex::new(None),
            tx,
            tap: AtomicPtr::new(std::ptr::null_mut()),
        });

        let state_c = Arc::clone(&state);
        std::thread::spawn(move || {
            match create_tap(&state_c) {
                Ok(()) => {
                    let _ = handshake_tx.send(Ok(()));
                    // 阻塞在这里，直到进程退出
                    unsafe { CFRunLoopRun() };
                }
                Err(e) => {
                    let _ = handshake_tx.send(Err(e));
                }
            }
        });

        // 等后台线程把 tap 建好（失败则把错误带回来）
        match handshake_rx.recv() {
            Ok(Ok(())) => Ok(HotkeyListener { rx }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Other("热键监听线程异常退出".into())),
        }
    }

    /// 阻塞等待下一个事件。
    pub fn next(&self) -> Option<HotkeyEvent> {
        self.rx.recv().ok()
    }

    /// 带超时的等待。
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<HotkeyEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

/// 创建 CGEventTap 并挂到当前线程的 run loop 上。
fn create_tap(state: &Arc<TapState>) -> Result<()> {
    unsafe {
        // 只关心按键按下 / 抬起 / 修饰键变化
        let mask = (1u64 << EVENT_KEY_DOWN)
            | (1u64 << EVENT_KEY_UP)
            | (1u64 << EVENT_FLAGS_CHANGED);

        let tap = CGEventTapCreate(
            TAP_SESSION,
            TAP_HEAD_INSERT,
            TAP_LISTEN_ONLY,
            mask,
            tap_callback,
            Arc::as_ptr(state) as *mut c_void,
        );
        if tap.is_null() {
            return Err(Error::Other(
                "创建全局热键监听失败：需要给 Vox 授予「辅助功能」权限（系统设置 › 隐私与安全性 › 辅助功能）"
                    .into(),
            ));
        }
        state.tap.store(tap as *mut c_void, Ordering::Relaxed);

        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        if source.is_null() {
            CFRelease(tap as CFTypeRef);
            return Err(Error::Other("创建 event tap 的 run loop source 失败".into()));
        }

        let rl = CFRunLoopGetCurrent();
        CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个带指定绑定的监听器状态，返回它和事件接收端。
    fn state() -> (TapState, Receiver<HotkeyEvent>) {
        let (tx, rx) = channel::<HotkeyEvent>();
        let bindings = vec![
            ("dictation".into(), KeyCombo::parse("RightCtrl").unwrap()),
            ("translate".into(), KeyCombo::parse("RightCtrl+RightShift").unwrap()),
            ("ask".into(), KeyCombo::parse("RightCtrl+Space").unwrap()),
        ];
        let master = vox_core::mode::MASTER_FEATURE.to_string();
        let master_keys = bindings
            .iter()
            .find(|(f, _)| *f == master)
            .map(|(_, c)| c.keys.clone())
            .unwrap_or_default();
        let state = TapState {
            bindings,
            master,
            master_keys,
            pressed: Mutex::new(HashSet::new()),
            active: Mutex::new(None),
            tx,
            tap: AtomicPtr::new(std::ptr::null_mut()),
        };
        (state, rx)
    }

    fn is_combo_down(e: &HotkeyEvent, feature: &str) -> bool {
        matches!(e, HotkeyEvent::ComboDown { feature: f, .. } if f == feature)
    }

    /// 真实键盘的修饰键只走 FlagsChanged：按下（标志开）→ ComboDown。
    #[test]
    fn flags_changed_press_then_release() {
        let (s, rx) = state();
        s.on_flags_changed(62, FLAG_CONTROL);
        let e = rx.try_recv().expect("按下应触发 ComboDown");
        assert!(is_combo_down(&e, "dictation"), "got {e:?}");

        s.on_flags_changed(62, 0);
        let e = rx.try_recv().expect("松开应触发 ComboUp");
        assert!(matches!(e, HotkeyEvent::ComboUp { .. }), "got {e:?}");
        assert!(rx.try_recv().is_err(), "不应该有额外事件");
    }

    /// 回归：系统同时补发 `KeyUp` 时，紧接着的 FlagsChanged **不能**把会话重新拉起来。
    ///
    /// 旧实现用「集合里有没有这个键」猜方向，KeyUp 先结束会话后，
    /// 紧接着的 FlagsChanged 会被误判成「按下」→ 录音重新开始且再也停不下来。
    #[test]
    fn duplicate_keyup_does_not_restart_session() {
        let (s, rx) = state();
        s.on_flags_changed(62, FLAG_CONTROL);
        assert!(rx.try_recv().is_ok()); // ComboDown

        s.on_release(62); // 补发的 KeyUp → 会话结束
        assert!(matches!(rx.try_recv(), Ok(HotkeyEvent::ComboUp { .. })));

        // 紧接着补发的 FlagsChanged（聚合标志已是关的）→ 必须被忽略
        s.on_flags_changed(62, 0);
        assert!(rx.try_recv().is_err(), "不应重新触发 ComboDown");
    }

    /// 加按另一个修饰键 → 切换模式（不是重新开始一次）。
    #[test]
    fn extra_modifier_switches_mode() {
        let (s, rx) = state();
        s.on_flags_changed(62, FLAG_CONTROL);
        assert!(is_combo_down(&rx.try_recv().unwrap(), "dictation"));

        s.on_flags_changed(60, FLAG_CONTROL | FLAG_SHIFT);
        assert!(is_combo_down(&rx.try_recv().unwrap(), "translate"));

        // 松开 Shift：**不**重新匹配 —— 模式保持 translate（锁存），不抖回 dictation
        s.on_flags_changed(60, FLAG_CONTROL);
        assert!(rx.try_recv().is_err(), "松开 Shift 不应产生事件（模式保持锁存）");

        // 再松开 Ctrl → 结束
        s.on_flags_changed(62, 0);
        assert!(matches!(rx.try_recv(), Ok(HotkeyEvent::ComboUp { .. })));
    }

    /// **主控键被按下时一定要发事件** —— 即使按下的集合没变。
    ///
    /// 这是「结束热键统一用 dictation 的热键」能生效的前提：
    /// 已经按住 RightCtrl+Space（Ask）时，松一下 RightCtrl 再按下去，
    /// 集合又变回 {RightCtrl, Space}（匹配没变），如果不发事件就永远停不下来。
    #[test]
    fn master_key_press_always_emits_even_if_set_unchanged() {
        let (s, rx) = state();
        // 按住 RightCtrl + Space → Ask
        s.on_flags_changed(62, FLAG_CONTROL);
        assert!(is_combo_down(&rx.try_recv().unwrap(), "dictation"));
        s.on_press(49);
        assert!(is_combo_down(&rx.try_recv().unwrap(), "ask"));

        // 松开 RightCtrl（集合变成 {Space}，不匹配任何绑定）→ 不发事件，模式保持
        s.on_release(62);
        assert!(rx.try_recv().is_err(), "集合不匹配任何绑定时不应发事件");

        // 再按 RightCtrl（集合变回 {RightCtrl,Space}，匹配没变，但主控键被按下）→ 必须发事件
        s.on_press(62);
        let e = rx
            .try_recv()
            .expect("主控键被按下必须发事件（哪怕匹配没变）");
        assert!(
            matches!(e, HotkeyEvent::ComboDown { master_pressed: true, .. }),
            "got {e:?}"
        );
    }

    /// 注入事件路径（只有 KeyDown/KeyUp）也要正常。
    #[test]
    fn injected_key_down_up_path() {
        let (s, rx) = state();
        s.on_press(62);
        assert!(is_combo_down(&rx.try_recv().unwrap(), "dictation"));
        s.on_release(62);
        assert!(matches!(rx.try_recv(), Ok(HotkeyEvent::ComboUp { .. })));
        assert!(rx.try_recv().is_err());
    }

    /// 普通键（空格）走 KeyDown/KeyUp：加按后切到 Ask。
    #[test]
    fn space_switches_to_ask() {
        let (s, rx) = state();
        s.on_flags_changed(62, FLAG_CONTROL);
        assert!(is_combo_down(&rx.try_recv().unwrap(), "dictation"));
        s.on_press(49); // Space
        assert!(is_combo_down(&rx.try_recv().unwrap(), "ask"));
        // 松开 Space：不重新匹配 —— 模式保持 Ask（锁存）
        s.on_release(49);
        assert!(rx.try_recv().is_err(), "松开 Space 不应产生事件（模式保持锁存）");
        s.on_flags_changed(62, 0);
        assert!(matches!(rx.try_recv(), Ok(HotkeyEvent::ComboUp { .. })));
    }

    #[test]
    fn parses_combo() {
        let c = KeyCombo::parse("RightCtrl+RightShift").unwrap();
        assert_eq!(c.keys.len(), 2);
        assert!(c.keys.contains(&Key::RightCtrl));
        assert!(c.keys.contains(&Key::RightShift));
    }

    #[test]
    fn parses_single_key() {
        let c = KeyCombo::parse("Space").unwrap();
        assert_eq!(c.keys, vec![Key::Space]);
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(KeyCombo::parse("NotAKey").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(KeyCombo::parse("  ").is_err());
    }

    #[test]
    fn rejects_duplicate_key() {
        assert!(KeyCombo::parse("RightCtrl+RightCtrl").is_err());
    }

    #[test]
    fn supports_context_menu() {
        // rdev 不支持这个键，我们自己实现后可以支持了
        let c = KeyCombo::parse("RightCtrl+ContextMenu").unwrap();
        assert!(c.keys.contains(&Key::ContextMenu));
    }

    #[test]
    fn keycode_roundtrip() {
        for key in [Key::RightCtrl, Key::RightShift, Key::Space, Key::A, Key::F12] {
            assert_eq!(Key::from_keycode(key.keycode()), Some(key));
        }
        assert_eq!(Key::from_keycode(0xFFFF), None);
    }

    #[test]
    fn matches_requires_exact_set() {
        let c = KeyCombo::parse("RightCtrl+RightShift").unwrap();
        let mut pressed = HashSet::new();
        pressed.insert(Key::RightCtrl);
        // 只有一个键时不匹配双键组合
        assert!(!c.matches_exactly(&pressed));
        pressed.insert(Key::RightShift);
        assert!(c.matches_exactly(&pressed));
        // 多按一个键就不匹配（否则会误触发上级绑定）
        pressed.insert(Key::Space);
        assert!(!c.matches_exactly(&pressed));
    }

    #[test]
    fn single_key_binding_ignores_extra_keys() {
        let c = KeyCombo::parse("RightCtrl").unwrap();
        let mut pressed = HashSet::new();
        pressed.insert(Key::RightCtrl);
        assert!(c.matches_exactly(&pressed));
        pressed.insert(Key::RightShift);
        assert!(!c.matches_exactly(&pressed));
    }
}
