#!/usr/bin/env python3
"""往当前系统注入按键事件，用来**自动验证** Vox 的全局热键（不用真人按键）。

注意：
- `CGEventPost` 注入的事件**只会产生 KeyDown / KeyUp**，不会产生 `FlagsChanged`。
  所以这个脚本**测不到**真实键盘的修饰键路径（Shift/Ctrl/Option/Cmd 只发 FlagsChanged），
  修饰键路径由 `crates/macos/src/hotkey.rs` 里的单测覆盖。
- 全屏播放的 App 可能挡住窗口截图，但不影响事件投递。

用法:
    python3 tools/post-key.py <keycode> [按住秒数]
    python3 tools/post-key.py 62 1.5      # 右 Ctrl，按住 1.5 秒
    python3 tools/post-key.py 60 0.2      # 右 Shift，点一下
    python3 tools/post-key.py 49          # 空格，点一下

常用 keycode: 62=右Ctrl 59=左Ctrl 60=右Shift 56=左Shift 54=右Cmd 55=左Cmd
             49=空格 36=回车 51=退格 63=Fn 110=菜单键
"""

import ctypes
import sys
import time

CORE_GRAPHICS = "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics"
KCGHID_EVENT_TAP = 0  # 0=HID 1=Session


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    try:
        code = int(sys.argv[1])
    except ValueError:
        print("keycode 必须是数字")
        return 1
    hold = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0

    cg = ctypes.CDLL(CORE_GRAPHICS)
    cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
    cg.CGEventCreateKeyboardEvent.argtypes = [
        ctypes.c_uint32,
        ctypes.c_uint16,
        ctypes.c_bool,
    ]
    cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]

    def post(down: bool) -> None:
        ev = cg.CGEventCreateKeyboardEvent(KCGHID_EVENT_TAP, code, down)
        if not ev:
            print("CGEventCreateKeyboardEvent 返回空")
            return
        cg.CGEventPost(KCGHID_EVENT_TAP, ev)
        time.sleep(0.04)

    post(True)
    time.sleep(hold)
    post(False)
    print(f"posted keycode {code}, hold {hold}s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
