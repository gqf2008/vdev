#![no_std]
//! vdev 虚拟 HID（Windows）：基于 Virtual HID Framework (VHF) 的虚拟键盘 / 虚拟鼠标。
//!
//! ## 设备模型（Win10 1607+ 与 Win11 通用）
//!
//! - `Root\vdev-hid`（vdev 虚拟键盘）与 `Root\vdev-hid-mouse`（vdev 虚拟鼠标）仍由 INF
//!   建两个根枚举设备，显示名与硬件 ID 不变；本驱动是它们的**函数驱动**（KMDF FDO）。
//! - `EvtDevicePrepareHardware` 里用 `VhfCreate` + `VhfStart` 各创建一个虚拟 HID 设备：
//!   报告描述符与 VID/PID 交给系统 VHF（vhf.sys），由 hidclass 枚举成真实 HID 设备并
//!   发布 HID 设备接口；`EvtDeviceReleaseHardware` 里 `VhfDelete` 回收。
//! - 注入沿用"厂商输出报告"管道：用户态对 HID 接口 `WriteFile` 写 8 字节（键盘）/
//!   4 字节（鼠标）报告 → hidclass → VHF 的 WriteReport 回调 → 驱动把同一份字节作为
//!   **输入报告**用 `VhfReadReportSubmit` 交回，成为系统输入事件（与真实键鼠无异）。
//!
//! ## 为什么不用 mshidkmdf 路线（上游原设计；本机 Win10 19045 实测结论）
//!
//! vhidmini2(KMDF) 的写法依赖 `MsHidKmdf.inf`，而它只随 Windows 11（build 22000）提供：
//! Win10 上 `Include=MsHidKmdf.inf` 直接报 0xE0000219（找不到 include INF / 无
//! ASSOCSERVICE）；把系统自带的 mshidkmdf 手工声明成函数驱动或上层过滤器，设备也只到
//! `CM_PROB_FAILED_START` + 0xC000009C——**把本驱动整条从设备栈里摘掉后失败依旧**，
//! 证明症结在 mshidkmdf 无法服务 Win10 的 Root 枚举 HIDClass 设备。
//! VHF 是微软为"创建虚拟 HID 设备"提供的官方框架，Win10 1607 起随系统提供
//! （本机 `vhf.sys` = 10.0.19041.1），Win11 通用。

mod hid;

use core::cell::UnsafeCell;
use core::mem::size_of;

use hid::{KEYBOARD_REPORT_DESCRIPTOR, MOUSE_REPORT_DESCRIPTOR};
use wdk_sys::hid::{
    VhfAsyncOperationComplete, VhfCreate, VhfDelete, VhfReadReportSubmit, VhfStart,
};
use wdk_sys::{
    DRIVER_OBJECT, HID_XFER_PACKET, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, PVOID, PWSTR,
    ULONG, UNICODE_STRING, USHORT, VHF_CONFIG, VHFHANDLE, VHFOPERATIONHANDLE, WDF_DRIVER_CONFIG,
    WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES, WDF_PNPPOWER_EVENT_CALLBACKS, WDFCMRESLIST, WDFDEVICE,
    WDFDEVICE_INIT, WDFDRIVER, WDFKEY, WDFSPINLOCK, call_unsafe_wdf_function_binding,
};

/// HID 设备标识：VID "VD"；键盘 PID "HI"，鼠标 PID "HM"
const VID: u16 = 0x5644;
const PID_KBD: u16 = 0x4849;
const PID_MOUSE: u16 = 0x484D;
/// 设备版本号（HID_DEVICE_ATTRIBUTES.VersionNumber）
const DEVICE_VERSION: u16 = 0x0100;

/// NTSTATUS 常量（wdk-sys 未生成；值依 ntstatus.h）
const STATUS_SUCCESS: NTSTATUS = 0;
const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as NTSTATUS;
/// ntstatus.h STATUS_INVALID_BUFFER_SIZE = 0xC0000206
const STATUS_INVALID_BUFFER_SIZE: NTSTATUS = 0xC000_0206u32 as NTSTATUS;
// bugcodes.h MANUALLY_INITIATED_CRASH：panic 处理器 bugcheck 码
const MANUALLY_INITIATED_CRASH: u32 = 0x0000_00E2;

/// 键盘输入报告：8 字节（1 修饰键 + 1 保留 + 6 按键）
const KBD_REPORT_SIZE: usize = 8;
/// 鼠标输入报告：4 字节（1 键位 + X + Y + 滚轮）
const MOUSE_REPORT_SIZE: usize = 4;

/// INF 注册表 Role 值（0=键盘，1=鼠标）
const ROLE_MOUSE: u32 = 1;
/// PLUGPLAY_REGKEY_DEVICE = 1；KEY_READ = 0x20019
const PLUGPLAY_REGKEY_DEVICE: u32 = 0x0000_0001;
const KEY_READ: u32 = 0x0002_0019;

/// 设备角色：同一个驱动服务两个根枚举设备（键盘 / 鼠标）
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Keyboard,
    Mouse,
}

impl Role {
    /// 作为 VHF 客户上下文传给 VHF，回调里取回角色（VHF 只原样回传，不解引用）
    fn as_context(self) -> PVOID {
        (self as usize) as PVOID
    }

    fn from_context(context: PVOID) -> Role {
        if context as usize == ROLE_MOUSE as usize {
            Role::Mouse
        } else {
            Role::Keyboard
        }
    }

    fn report_descriptor(self) -> &'static [u8] {
        match self {
            Role::Keyboard => &KEYBOARD_REPORT_DESCRIPTOR,
            Role::Mouse => &MOUSE_REPORT_DESCRIPTOR,
        }
    }

    fn report_size(self) -> usize {
        match self {
            Role::Keyboard => KBD_REPORT_SIZE,
            Role::Mouse => MOUSE_REPORT_SIZE,
        }
    }

    fn product_id(self) -> u16 {
        match self {
            Role::Keyboard => PID_KBD,
            Role::Mouse => PID_MOUSE,
        }
    }
}

// ---------------- 设备状态：两个角色的 VHF 句柄，单锁保护 ----------------

struct Instance {
    /// VHF 虚拟 HID 设备句柄（VhfCreate 产出，VhfDelete 后置空）
    vhf: VHFHANDLE,
}

struct State {
    kbd: Instance,
    mouse: Instance,
}

struct StateCell {
    /// WDF 框架自旋锁：DriverEntry 中创建一次（早于一切回调，无并发写），之后只读
    spin_lock: UnsafeCell<WDFSPINLOCK>,
    state: UnsafeCell<State>,
}

// SAFETY: spin_lock 仅在 DriverEntry（单线程、无并发）中创建，之后只读；
// state 仅在持有 WdfSpinLock（WdfSpinLockAcquire/Release 临界区）时访问；
// VHFHANDLE 是框架句柄，可跨线程共享
unsafe impl Sync for StateCell {}

const fn new_instance() -> Instance {
    Instance {
        vhf: core::ptr::null_mut(),
    }
}

static STATE: StateCell = StateCell {
    spin_lock: UnsafeCell::new(core::ptr::null_mut()),
    state: UnsafeCell::new(State {
        kbd: new_instance(),
        mouse: new_instance(),
    }),
};

impl StateCell {
    /// DriverEntry：创建 KMDF 框架自旋锁。未给对象属性时（WDF_NO_OBJECT_ATTRIBUTES）
    /// 其父对象默认为 framework driver object，生命周期等同驱动。
    /// 选用 WdfSpinLock 而非手工 KSPIN_LOCK：IRQL 提升与恢复由框架处理，
    /// 临界区内只做内存读写，不需要 PASSIVE_LEVEL 等待语义。
    fn init(&self) -> NTSTATUS {
        let mut lock: WDFSPINLOCK = core::ptr::null_mut();
        // SAFETY: 空对象属性合法；lock 输出指针有效
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfSpinLockCreate,
                WDF_NO_OBJECT_ATTRIBUTES,
                &mut lock
            )
        };
        if status < 0 {
            return status;
        }
        // SAFETY: DriverEntry 早于一切并发回调，此处独占写
        unsafe {
            *self.spin_lock.get() = lock;
        }
        STATUS_SUCCESS
    }

    /// 获取自旋锁并返回 RAII 守卫（Drop 时 Release）
    fn lock(&self) -> StateGuard<'_> {
        // SAFETY: init 之后只读
        let lock = unsafe { *self.spin_lock.get() };
        // SAFETY: lock 为 init 创建的有效句柄；Acquire 自动提升 IRQL 到 DISPATCH_LEVEL
        unsafe {
            call_unsafe_wdf_function_binding!(WdfSpinLockAcquire, lock);
        }
        StateGuard(self)
    }
}

/// 自旋锁守卫（RAII 释放）
struct StateGuard<'a>(&'a StateCell);

impl core::ops::Deref for StateGuard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        // SAFETY: 已持有自旋锁，临界区内唯一访问者
        unsafe { &*self.0.state.get() }
    }
}

impl core::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        // SAFETY: 已持有自旋锁，临界区内唯一访问者
        unsafe { &mut *self.0.state.get() }
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: 与 WdfSpinLockAcquire 成对；Release 恢复原 IRQL
        let lock = unsafe { *self.0.spin_lock.get() };
        unsafe {
            call_unsafe_wdf_function_binding!(WdfSpinLockRelease, lock);
        }
    }
}

fn instance_mut(state: &mut State, role: Role) -> &mut Instance {
    match role {
        Role::Keyboard => &mut state.kbd,
        Role::Mouse => &mut state.mouse,
    }
}

/// 取角色的 VHF 句柄（未创建时为空）
fn vhf_handle(role: Role) -> VHFHANDLE {
    let g = STATE.lock();
    let inst = match role {
        Role::Keyboard => &g.kbd,
        Role::Mouse => &g.mouse,
    };
    inst.vhf
}

/// 记录角色的 VHF 句柄（空指针表示已回收）
fn set_vhf_handle(role: Role, handle: VHFHANDLE) {
    let mut g = STATE.lock();
    instance_mut(&mut g, role).vhf = handle;
}

// ---------------- 设备添加 / 硬件准备 ----------------

/// 读取设备硬件键注册表的 Role 值（INF 写入；缺省键盘）
fn read_role(device: WDFDEVICE) -> Role {
    let mut key: WDFKEY = core::ptr::null_mut();
    // SAFETY: device 有效；WDF_NO_OBJECT_ATTRIBUTES 允许空属性
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceOpenRegistryKey,
            device,
            PLUGPLAY_REGKEY_DEVICE,
            KEY_READ,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut key
        )
    };
    if status < 0 || key.is_null() {
        return Role::Keyboard;
    }
    static ROLE_NAME: [u16; 5] = ['R' as u16, 'o' as u16, 'l' as u16, 'e' as u16, 0];
    let mut value_name = UNICODE_STRING {
        Length: 8, // 4 chars * 2
        MaximumLength: (ROLE_NAME.len() * 2) as u16,
        Buffer: ROLE_NAME.as_ptr().cast_mut() as PWSTR,
    };
    let mut value: u32 = 0;
    // SAFETY: key 有效；value_name 指向静态宽字符串；value 可写
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfRegistryQueryULong, key, &mut value_name, &mut value)
    };
    // SAFETY: 关闭注册表键
    unsafe {
        call_unsafe_wdf_function_binding!(WdfRegistryClose, key);
    }
    if status >= 0 && value == ROLE_MOUSE {
        Role::Mouse
    } else {
        Role::Keyboard
    }
}

/// 设备添加回调：创建 KMDF 设备对象（FDO）并登记 PnP/Power 回调
///
/// VHF 设备的创建放在 `EvtDevicePrepareHardware`（需 PASSIVE_LEVEL，且与
/// `EvtDeviceReleaseHardware` 成对，设备被停止/重启后可重建）。
///
/// # Safety
/// 由 WDF 调用，`init` 为框架提供的有效 `WDFDEVICE_INIT`。
unsafe extern "C" fn evt_device_add(_driver: WDFDRIVER, mut init: *mut WDFDEVICE_INIT) -> NTSTATUS {
    let mut pnp_power = WDF_PNPPOWER_EVENT_CALLBACKS {
        Size: size_of::<WDF_PNPPOWER_EVENT_CALLBACKS>() as ULONG,
        EvtDevicePrepareHardware: Some(evt_prepare_hardware),
        EvtDeviceSelfManagedIoInit: Some(evt_self_managed_io_init),
        EvtDeviceSelfManagedIoCleanup: Some(evt_self_managed_io_cleanup),
        ..WDF_PNPPOWER_EVENT_CALLBACKS::default()
    };
    // SAFETY: init 有效；pnp_power 在 WdfDeviceCreate 前注册（框架会拷贝）
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetPnpPowerEventCallbacks,
            init,
            &mut pnp_power
        );
    }

    let mut device: WDFDEVICE = core::ptr::null_mut();
    // 设备属性传 WDF_NO_OBJECT_ATTRIBUTES（NULL），与 windows-drivers-rs 官方样例一致。
    // 反面案例（Win10 实测）：手工构造 WDF_OBJECT_ATTRIBUTES{Size: size_of::<..>()} 会被
    // KMDF 运行库判为非法，WdfDeviceCreate 返回 STATUS_WDF_OBJECT_ATTRIBUTES_INVALID
    // (0xC0200209)，设备停在 CM_PROB_FAILED_ADD。本驱动无需设备上下文/清理回调，
    // 生命周期钩子改由 PnP/Power 回调承担。
    // SAFETY: init/device 有效；属性允许为空
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &mut init,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut device
        )
    };
    if status < 0 {
        return status;
    }
    STATUS_SUCCESS
}

/// 准备硬件：按角色创建并启动 VHF 虚拟 HID 设备
///
/// # Safety
/// 由 WDF 调用，`device` 为有效句柄；两个资源列表未使用。
unsafe extern "C" fn evt_prepare_hardware(
    device: WDFDEVICE,
    _resources_raw: WDFCMRESLIST,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    let role = read_role(device);
    // 设备被停止后重启时会再次进来；VHF 设备仍然活着，直接复用（见 cleanup 回调）
    if !vhf_handle(role).is_null() {
        return STATUS_SUCCESS;
    }
    let descriptor = role.report_descriptor();

    // SAFETY: VHF_CONFIG 是纯 POD（裸指针 + GUID），全零后逐字段显式赋值即合法初值
    let mut config: VHF_CONFIG = unsafe { core::mem::zeroed() };
    config.Size = size_of::<VHF_CONFIG>() as ULONG;
    // SAFETY: device 有效
    config.DeviceObject =
        unsafe { call_unsafe_wdf_function_binding!(WdfDeviceWdmGetDeviceObject, device) };
    config.VendorID = VID;
    config.ProductID = role.product_id();
    config.VersionNumber = DEVICE_VERSION;
    config.ReportDescriptorLength = descriptor.len() as USHORT;
    config.ReportDescriptor = descriptor.as_ptr().cast_mut();
    config.VhfClientContext = role.as_context();
    // 只注册 WriteReport：用户态写厂商输出报告即注入。
    // 不注册 EvtVhfReadyForNextReadReport → 走 VHF 默认缓冲策略，提交无时序约束。
    config.EvtVhfAsyncOperationWriteReport = Some(evt_vhf_write_report);

    let mut vhf: VHFHANDLE = core::ptr::null_mut();
    // SAFETY: config 有效；描述符为静态数据，生命周期覆盖 VhfCreate/VhfStart 全程
    let status = unsafe { VhfCreate(&mut config, &mut vhf) };
    if status < 0 {
        return status;
    }
    set_vhf_handle(role, vhf);

    // SAFETY: vhf 由 VhfCreate 产出且尚未删除
    let status = unsafe { VhfStart(vhf) };
    if status < 0 {
        // SAFETY: 启动失败则回收刚创建的 VHF 设备（PASSIVE_LEVEL，等待删除完成）
        unsafe { VhfDelete(vhf, 1) };
        set_vhf_handle(role, core::ptr::null_mut());
        return status;
    }
    STATUS_SUCCESS
}

/// 自管理 I/O 初始化：本驱动没有自管理 I/O，仅用于与 Cleanup 成对。
///
/// # Safety
/// 由 WDF 调用。
unsafe extern "C" fn evt_self_managed_io_init(_device: WDFDEVICE) -> NTSTATUS {
    STATUS_SUCCESS
}

/// 自管理 I/O 清理（设备移除阶段，晚于 ReleaseHardware）：回收 VHF 虚拟 HID 设备。
///
/// 不在 ReleaseHardware 里删：那是"停止设备"路径，此时 HID 子设备仍在，
/// `VhfDelete(..., TRUE)` 等待 VHF 释放会卡住，表现为 `pnputil /remove-device` 长时间
/// 无响应（实测）。放到清理阶段后，虚拟 HID 设备先随 PnP 移除，再删 VHF 句柄。
///
/// # Safety
/// 由 WDF 在设备移除清理阶段（PASSIVE_LEVEL）调用，`device` 为有效句柄。
unsafe extern "C" fn evt_self_managed_io_cleanup(device: WDFDEVICE) {
    let role = read_role(device);
    let handle = vhf_handle(role);
    set_vhf_handle(role, core::ptr::null_mut());
    if !handle.is_null() {
        // SAFETY: handle 由 VhfCreate 产出且未删除；PASSIVE_LEVEL 下 Wait=TRUE 合法
        unsafe { VhfDelete(handle, 1) };
    }
}

// ---------------- 注入：写厂商输出报告 → 交回输入报告 ----------------

/// VHF WriteReport 回调：hidclass 收到 HID 客户端的输出报告（来自用户态 WriteFile）。
///
/// 本驱动把这份字节原样当**输入报告**投递（键盘 8 字节 / 鼠标 4 字节，与报告描述符里
/// 8/4 字节的厂商输出管道一一对应），于是系统把注入当成真实键鼠输入。
///
/// # Safety
/// 由 VHF 调用：`operation` 为本次操作句柄，`packet` 指向 VHF 提供的传输包。
unsafe extern "C" fn evt_vhf_write_report(
    context: PVOID,
    operation: VHFOPERATIONHANDLE,
    _operation_context: PVOID,
    packet: wdk_sys::PHID_XFER_PACKET,
) {
    let status = unsafe { submit_as_input_report(context, packet) };
    // SAFETY: operation 由 VHF 提供；同一次异步操作恰好完成一次，返回值无需处理
    unsafe {
        let _ = VhfAsyncOperationComplete(operation, status);
    }
}

/// 取 WriteReport 包里的报告字节，按角色原样作为**输入报告**交回 VHF。
///
/// # Safety
/// `packet` 由 VHF 在回调期间提供；本函数检查空指针与长度。
unsafe fn submit_as_input_report(context: PVOID, packet: wdk_sys::PHID_XFER_PACKET) -> NTSTATUS {
    let role = Role::from_context(context);
    let size = role.report_size();
    if packet.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: packet 非空且由 VHF 提供，回调期间有效
    let (buffer, length, report_id) = unsafe {
        (
            (*packet).reportBuffer,
            (*packet).reportBufferLen as usize,
            (*packet).reportId,
        )
    };
    if buffer.is_null() || length < size {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    let handle = vhf_handle(role);
    if handle.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let mut report = [0u8; KBD_REPORT_SIZE];
    // SAFETY: buffer 至少 length >= size 字节可读
    unsafe {
        core::ptr::copy_nonoverlapping(buffer, report.as_mut_ptr(), size);
    }
    let mut xfer = HID_XFER_PACKET {
        reportBuffer: report.as_mut_ptr(),
        reportBufferLen: size as ULONG,
        reportId: report_id,
    };
    // SAFETY: handle 有效；xfer 指向的缓冲在调用返回前一直有效
    unsafe { VhfReadReportSubmit(handle, &mut xfer) }
}

// ---------------- 驱动入口 ----------------

/// Windows 驱动入口点
///
/// # Safety
/// 由加载器调用，参数为内核传入的有效指针。
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver: *mut DRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    let mut config = WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as ULONG,
        EvtDriverDeviceAdd: Some(evt_device_add),
        ..WDF_DRIVER_CONFIG::default()
    };
    // SAFETY: driver/registry_path 由 DriverEntry 提供且有效；config 有效；handle 输出可空
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver as PDRIVER_OBJECT,
            registry_path,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut config,
            WDF_NO_HANDLE.cast::<WDFDRIVER>(),
        )
    };
    if status < 0 {
        return status;
    }
    // 初始化全局 WDF 自旋锁（早于一切设备/IO 回调）
    STATE.init()
}

/// panic 处理器：内核态 panic 无法 unwind，自旋=静默挂死；直接 bugcheck 留下蓝屏证据。
/// BugCheck 码取 bugcodes.h 的 MANUALLY_INITIATED_CRASH (0xE2)，
/// 参数 1 传 PanicInfo 指针供调试器/转储定位。
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // SAFETY: bugcheck 为终态调用（不返回），参数仅承载诊断信息
    unsafe {
        wdk_sys::ntddk::KeBugCheckEx(
            MANUALLY_INITIATED_CRASH as _,
            (info as *const core::panic::PanicInfo).addr() as _,
            0,
            0,
            0,
        );
    }
}
