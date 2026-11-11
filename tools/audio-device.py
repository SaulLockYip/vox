#!/usr/bin/env python3
"""查看 / 切换 macOS 的默认音频设备（诊断蓝牙耳机「卡在 HFP」用）。

蓝牙耳机一旦被打开麦克风，链路会从 **A2DP**（立体声 44.1/48k）切到
**HFP**（单声道 16k）。麦克风释放后 macOS 通常会自己切回 A2DP，
但有时会**卡住不切** —— 表现就是「录音结束了，音质还是差的」。

用法：
    python3 tools/audio-device.py list              # 列出设备
    python3 tools/audio-device.py status            # 看默认输出的声道/采样率
    python3 tools/audio-device.py set-output "名字"   # 切默认输出设备
"""

import ctypes
import sys

cf = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
cf.CFStringCreateWithCString.restype = ctypes.c_void_p
cf.CFStringCreateWithCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
cf.CFStringGetCString.restype = ctypes.c_int32
cf.CFStringGetCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_long, ctypes.c_uint32]
cf.CFStringGetLength.restype = ctypes.c_long
cf.CFStringGetLength.argtypes = [ctypes.c_void_p]

ca = ctypes.CDLL("/System/Library/Frameworks/CoreAudio.framework/CoreAudio")
ca.AudioObjectGetPropertyData.restype = ctypes.c_int32
ca.AudioObjectGetPropertyData.argtypes = [
    ctypes.c_uint32, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p,
    ctypes.POINTER(ctypes.c_uint32), ctypes.c_void_p,
]
ca.AudioObjectSetPropertyData.restype = ctypes.c_int32
ca.AudioObjectSetPropertyData.argtypes = [
    ctypes.c_uint32, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p,
    ctypes.c_uint32, ctypes.c_void_p,
]

UTF8 = 0x08000100
kAudioObjectSystemObject = 1
kAudioHardwarePropertyDevices = "dev#"
kAudioHardwarePropertyDefaultOutputDevice = "dOut"
kAudioObjectPropertyName = "lnam"
kAudioDevicePropertyStreamConfiguration = "slay"
kAudioObjectPropertyScopeGlobal = "glob"
kAudioObjectPropertyElementMain = 0


class Address(ctypes.Structure):
    """AudioObjectPropertyAddress：selector + scope + element。"""

    _fields_ = [
        ("mSelector", ctypes.c_void_p),
        ("mScope", ctypes.c_void_p),
        ("mElement", ctypes.c_uint32),
    ]


def S(s: str):
    return cf.CFStringCreateWithCString(None, s.encode(), UTF8)


def addr(prop: str) -> Address:
    return Address(mSelector=S(prop), mScope=S(kAudioObjectPropertyScopeGlobal),
                   mElement=kAudioObjectPropertyElementMain)


def get_data(obj, prop, out_type):
    a = addr(prop)
    size = ctypes.c_uint32(0)
    st = ca.AudioObjectGetPropertyData(obj, ctypes.byref(a), 0, None, ctypes.byref(size), None)
    if st != 0:
        return None
    buf = (out_type * (size.value // ctypes.sizeof(out_type)))()
    st = ca.AudioObjectGetPropertyData(obj, ctypes.byref(a), 0, None, ctypes.byref(size), buf)
    if st != 0:
        return None
    return buf


def device_name(obj) -> str:
    ref = ctypes.c_void_p()
    size = ctypes.c_uint32(ctypes.sizeof(ref))
    a = addr(kAudioObjectPropertyName)
    st = ca.AudioObjectGetPropertyData(obj, ctypes.byref(a), 0, None, ctypes.byref(size),
                                        ctypes.byref(ref))
    if st != 0 or not ref:
        return "(未知)"
    n = cf.CFStringGetLength(ref)
    buf = ctypes.create_string_buffer(n * 4 + 8)
    cf.CFStringGetCString(ref, buf, len(buf), UTF8)
    return buf.value.decode("utf-8", "replace")


def stream_channels(obj) -> int:
    """输出声道数：A2DP=2，HFP=1。"""
    buf = get_data(obj, kAudioDevicePropertyStreamConfiguration, ctypes.c_uint8)
    if not buf or len(buf) < 12:
        return 0
    n_buffers = int.from_bytes(bytes(buf[0:4]), "little")
    if n_buffers < 1:
        return 0
    return int.from_bytes(bytes(buf[8:12]), "little")


def sample_rate(obj) -> int:
    buf = get_data(obj, "nsrt", ctypes.c_uint8)
    if not buf or len(buf) < 8:
        return 0
    return int.from_bytes(bytes(buf[4:8]), "little") >> 16


def devices():
    out = []
    ids = get_data(kAudioObjectSystemObject, kAudioHardwarePropertyDevices, ctypes.c_uint32)
    if not ids:
        return out
    for oid in ids:
        out.append((oid.value, device_name(oid)))
    return out


def default_output():
    buf = get_data(kAudioObjectSystemObject, kAudioHardwarePropertyDefaultOutputDevice, ctypes.c_uint32)
    return buf[0] if buf else None


def set_default_output(oid):
    val = ctypes.c_uint32(oid)
    size = ctypes.c_uint32(4)
    a = addr(kAudioHardwarePropertyDefaultOutputDevice)
    return ca.AudioObjectSetPropertyData(
        kAudioObjectSystemObject, ctypes.byref(a), 0, None, size, ctypes.byref(val),
    )


def describe(oid) -> str:
    return f"{device_name(oid)}  ch={stream_channels(oid)}  rate={sample_rate(oid)}"


def main() -> int:
    cmd = sys.argv[1] if len(sys.argv) > 1 else "status"
    if cmd == "list":
        for oid, name in devices():
            print(f"  {oid:<12} {name}")
        return 0
    if cmd == "status":
        oid = default_output()
        if oid is None:
            print("读不到默认输出设备")
            return 1
        print(f"默认输出: {describe(oid)}")
        print("  ch=2 → A2DP 立体声（音质正常）; ch=1 → HFP 单声道（蓝牙通话模式，音质差）")
        return 0
    if cmd == "set-output":
        want = sys.argv[2]
        for oid, name in devices():
            if name == want:
                st = set_default_output(oid)
                print(f"设置默认输出 = {name} (status={st})")
                return 0 if st == 0 else 1
        print(f"找不到设备: {want}")
        return 1
    print(__doc__)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
