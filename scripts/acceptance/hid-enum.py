"""枚举 HID 设备接口，打印「读写打开 / 只写打开」结果与 VID/PID。

用途：确认 vdev 的虚拟键鼠真的以 HID 接口暴露出来（VID 0x5644 = 'VD'，键盘 PID 0x4849 = 'HI'，
鼠标 PID 0x484D = 'HM'），以及用户态能不能打开——这是"注入会不会生效"的前置条件。

用法：python scripts/acceptance/hid-enum.py
（枚举结束时的 err=259 是 ERROR_NO_MORE_ITEMS，正常终止；rw=err5 是 ACCESS_DENIED，
说明该接口只允许只写打开——CLI 走的就是只写。）
"""
import ctypes
from ctypes import wintypes

setupapi = ctypes.WinDLL("setupapi", use_last_error=True)
hid = ctypes.WinDLL("hid", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)


class GUID(ctypes.Structure):
    _fields_ = [("d1", wintypes.DWORD), ("d2", wintypes.WORD),
                ("d3", wintypes.WORD), ("d4", ctypes.c_ubyte * 8)]


class SP_DEVICE_INTERFACE_DATA(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("InterfaceClassGuid", GUID),
                ("Flags", wintypes.DWORD), ("Reserved", ctypes.POINTER(ctypes.c_ulong))]


hid.HidD_GetHidGuid.argtypes = [ctypes.POINTER(GUID)]
setupapi.SetupDiGetClassDevsW.restype = wintypes.HANDLE
setupapi.SetupDiGetClassDevsW.argtypes = [ctypes.POINTER(GUID), wintypes.LPCWSTR,
                                         wintypes.HWND, wintypes.DWORD]
setupapi.SetupDiEnumDeviceInterfaces.argtypes = [wintypes.HANDLE, ctypes.c_void_p,
                                                 ctypes.POINTER(GUID), wintypes.DWORD,
                                                 ctypes.POINTER(SP_DEVICE_INTERFACE_DATA)]
setupapi.SetupDiGetDeviceInterfaceDetailW.argtypes = [
    wintypes.HANDLE, ctypes.POINTER(SP_DEVICE_INTERFACE_DATA), ctypes.c_void_p,
    wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
kernel32.CreateFileW.restype = wintypes.HANDLE
kernel32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                 ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
hid.HidD_GetAttributes.argtypes = [wintypes.HANDLE, ctypes.c_void_p]

GENERIC_READ, GENERIC_WRITE = 0x80000000, 0x40000000
OPEN_EXISTING, FILE_SHARE_RW = 3, 3
DIGCF_PRESENT, DIGCF_DEVICEINTERFACE = 0x2, 0x10
INVALID = ctypes.c_void_p(-1).value


def open_dev(path, access):
    ctypes.set_last_error(0)
    h = kernel32.CreateFileW(path, access, FILE_SHARE_RW, None, OPEN_EXISTING, 0, None)
    err = ctypes.get_last_error()
    if h == INVALID or h is None or h == 0:
        return None, err
    return h, 0


guid = GUID()
hid.HidD_GetHidGuid(ctypes.byref(guid))
devs = setupapi.SetupDiGetClassDevsW(ctypes.byref(guid), None, None,
                                     DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
print(f"hdevinfo={devs:#x}")

idx = 0
n = 0
while True:
    ifd = SP_DEVICE_INTERFACE_DATA()
    ifd.cbSize = ctypes.sizeof(SP_DEVICE_INTERFACE_DATA)
    if not setupapi.SetupDiEnumDeviceInterfaces(devs, None, ctypes.byref(guid), idx, ctypes.byref(ifd)):
        print(f"enum stop at index {idx}: err={ctypes.get_last_error()}")
        break
    idx += 1
    need = wintypes.DWORD(0)
    setupapi.SetupDiGetDeviceInterfaceDetailW(devs, ctypes.byref(ifd), None, 0,
                                              ctypes.byref(need), None)
    buf = ctypes.create_string_buffer(need.value + 32)
    ctypes.memmove(buf, ctypes.byref(wintypes.DWORD(ctypes.sizeof(wintypes.DWORD) * 2)), 4)
    if not setupapi.SetupDiGetDeviceInterfaceDetailW(devs, ctypes.byref(ifd), buf,
                                                     need.value, ctypes.byref(need), None):
        continue
    path = ctypes.wstring_at(ctypes.addressof(buf) + 4)
    n += 1

    h_rw, err_rw = open_dev(path, GENERIC_READ | GENERIC_WRITE)
    h_w, err_w = open_dev(path, GENERIC_WRITE)
    h = h_rw or h_w
    vid = pid = None
    if h:
        attr = ctypes.create_string_buffer(16)
        if hid.HidD_GetAttributes(h, attr):
            vid = int.from_bytes(attr[4:6], "little")
            pid = int.from_bytes(attr[6:8], "little")
    if h_rw:
        kernel32.CloseHandle(h_rw)
    if h_w:
        kernel32.CloseHandle(h_w)

    if vid == 0x5644 or "VHF" in path.upper() or "5644" in path.upper():
        print(f"[{n}] VID={vid} PID={pid} rw={'ok' if h_rw else f'err{err_rw}'} "
              f"w={'ok' if h_w else f'err{err_w}'}\n     {path}")
    else:
        print(f"[{n}] VID={vid} PID={pid} (other)")

print(f"total interfaces: {n}")
