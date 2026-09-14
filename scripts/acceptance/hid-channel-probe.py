"""逐条通道试写 vdev 鼠标报告，看哪条真的能让光标动（区分"写没到驱动"与"到了没进系统"）。

通道：A) HidD_SetFeature 带 1 字节 Report ID 前缀  B) HidD_SetFeature 裸报告  C) WriteFile(输出报告)
每次写完量光标位移；报文体 = [buttons, dx, dy, wheel]（与 CLI 的 mouse_report 一致）。

用法：python scripts/acceptance/hid-channel-probe.py

判读（2026-09-14 实机基准，Win10 19045，驱动为当前 main 构建、节点干净时）：
  A SetFeature(帧化 5B)  api_ok=True  err=0   光标 dx=+22   ← 唯一可用的注入通道
  B SetFeature(裸 4B)    api_ok=False err=87（长度不匹配：FeatureReportByteLength 含 Report ID）
  C/D WriteFile          api_ok=False err=1（ERROR_INVALID_FUNCTION：描述符里没有 Output 报告）

若 A 也"err=0 但光标不动"，说明写虽然被受理、但没进系统——先按 README 的「注入不生效排查顺序」
清驱动包 + 重装（幽灵/重复节点会让写入落到无效的设备实例上）。
"""
import ctypes
import time
from ctypes import wintypes

setupapi = ctypes.WinDLL("setupapi", use_last_error=True)
hid = ctypes.WinDLL("hid", use_last_error=True)
user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)


class GUID(ctypes.Structure):
    _fields_ = [("d1", wintypes.DWORD), ("d2", wintypes.WORD), ("d3", wintypes.WORD),
                ("d4", ctypes.c_ubyte * 8)]


class SP_IFD(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("InterfaceClassGuid", GUID),
                ("Flags", wintypes.DWORD), ("Reserved", ctypes.POINTER(ctypes.c_ulong))]


hid.HidD_GetHidGuid.argtypes = [ctypes.POINTER(GUID)]
setupapi.SetupDiGetClassDevsW.restype = wintypes.HANDLE
setupapi.SetupDiEnumDeviceInterfaces.argtypes = [wintypes.HANDLE, ctypes.c_void_p,
                                                ctypes.POINTER(GUID), wintypes.DWORD,
                                                ctypes.POINTER(SP_IFD)]
setupapi.SetupDiGetDeviceInterfaceDetailW.argtypes = [
    wintypes.HANDLE, ctypes.POINTER(SP_IFD), ctypes.c_void_p, wintypes.DWORD,
    ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
kernel32.CreateFileW.restype = wintypes.HANDLE
kernel32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                 ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
hid.HidD_GetAttributes.argtypes = [wintypes.HANDLE, ctypes.c_void_p]
hid.HidD_SetFeature.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.ULONG]
user32.GetCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]

INVALID = ctypes.c_void_p(-1).value


def cursor():
    p = wintypes.POINT()
    user32.GetCursorPos(ctypes.byref(p))
    return p.x, p.y


def find_mouse_paths():
    g = GUID()
    hid.HidD_GetHidGuid(ctypes.byref(g))
    devs = setupapi.SetupDiGetClassDevsW(ctypes.byref(g), None, None, 0x12)
    out, i = [], 0
    while True:
        ifd = SP_IFD()
        ifd.cbSize = ctypes.sizeof(SP_IFD)
        if not setupapi.SetupDiEnumDeviceInterfaces(devs, None, ctypes.byref(g), i, ctypes.byref(ifd)):
            break
        i += 1
        need = wintypes.DWORD(0)
        setupapi.SetupDiGetDeviceInterfaceDetailW(devs, ctypes.byref(ifd), None, 0,
                                                 ctypes.byref(need), None)
        buf = ctypes.create_string_buffer(need.value + 32)
        ctypes.memmove(buf, ctypes.byref(wintypes.DWORD(ctypes.sizeof(wintypes.DWORD) * 2)), 4)
        if not setupapi.SetupDiGetDeviceInterfaceDetailW(devs, ctypes.byref(ifd), buf, need.value,
                                                         ctypes.byref(need), None):
            continue
        path = ctypes.wstring_at(ctypes.addressof(buf) + 4)
        h = kernel32.CreateFileW(path, 0x40000000, 3, None, 3, 0, None)
        if h in (None, 0, INVALID):
            continue
        attr = ctypes.create_string_buffer(16)
        if hid.HidD_GetAttributes(h, attr):
            vid = int.from_bytes(attr[4:6], "little")
            pid = int.from_bytes(attr[6:8], "little")
            if vid == 0x5644 and pid == 0x484D:      # 鼠标
                out.append(path)
        kernel32.CloseHandle(h)
    return out


def attempt(tag, path, report, method):
    h = kernel32.CreateFileW(path, 0x40000000, 3, None, 3, 0, None)
    if h in (None, 0, INVALID):
        print(f"  {tag:34} 打开失败")
        return
    before = cursor()
    ctypes.set_last_error(0)
    if method == "feature":
        ok = hid.HidD_SetFeature(h, report, len(report))
    else:
        written = wintypes.DWORD(0)
        ok = kernel32.WriteFile(h, report, len(report), ctypes.byref(written), None)
    err = ctypes.get_last_error()
    kernel32.CloseHandle(h)
    time.sleep(0.8)
    after = cursor()
    print(f"  {tag:34} api_ok={bool(ok)} err={err} 光标 {before} -> {after} dx={after[0]-before[0]}")


def main():
    paths = find_mouse_paths()
    print(f"vdev 鼠标接口：{len(paths)} 个")
    for p in paths:
        print("   ", p)
        move = bytes([0x00, 0x14, 0x00, 0x00])          # buttons=0, dx=+20
        framed = bytes([0x00]) + move                    # 带 Report ID 前缀
        attempt("A SetFeature(帧化 5B)", p, framed, "feature")
        attempt("B SetFeature(裸 4B)", p, move, "feature")
        attempt("C WriteFile(裸 4B)", p, move, "output")
        attempt("D WriteFile(帧化 5B)", p, framed, "output")


if __name__ == "__main__":
    main()
