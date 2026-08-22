#![no_std]
//! vdev 虚拟 HID（路线 B）：KMDF 内核 HID minidriver（移植自 Microsoft vhidmini2）
//! 暴露两个 HID 设备：虚拟键盘（Boot Keyboard）与虚拟鼠标。
//! 用户态经 WriteFile 注入对应报告（键盘 8 字节 / 鼠标 4 字节），
//! 驱动把报告投递给 hidclass，被系统作为真实 HID 设备消费。

mod hid;

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

use hid::{
    HID_DESCRIPTOR, HID_DEVICE_ATTRIBUTES, HID_XFER_PACKET, KEYBOARD_REPORT_DESCRIPTOR,
    MOUSE_REPORT_DESCRIPTOR,
};
use wdk_sys::{
    DRIVER_OBJECT, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, PVOID, PWSTR, ULONG, UNICODE_STRING,
    WDF_DRIVER_CONFIG, WDF_IO_QUEUE_CONFIG, WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES, WDFDEVICE,
    WDFDEVICE_INIT, WDFDRIVER, WDFKEY, WDFMEMORY, WDFQUEUE, WDFREQUEST,
    call_unsafe_wdf_function_binding,
};

/// HID 设备标识：VID "VD"；键盘 PID "HI"，鼠标 PID "HM"
const VID: u16 = 0x5644;
const PID_KBD: u16 = 0x4849;
const PID_MOUSE: u16 = 0x484D;

/// NTSTATUS 常量（wdk-sys 未生成）
const STATUS_SUCCESS: NTSTATUS = 0;
const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010u32 as NTSTATUS;
const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as NTSTATUS;
const STATUS_INVALID_BUFFER_SIZE: NTSTATUS = 0xC000_020Cu32 as NTSTATUS;

/// 键盘输入报告：8 字节（1 修饰键 + 1 保留 + 6 按键）
const KBD_REPORT_SIZE: usize = 8;
/// 鼠标输入报告：4 字节（1 键位 + X + Y + 滚轮）
const MOUSE_REPORT_SIZE: usize = 4;

/// INF 注册表 Role 值（0=键盘，1=鼠标）
const ROLE_MOUSE: u32 = 1;
/// PLUGPLAY_REGKEY_DEVICE = 1；KEY_READ = 0x20019
const PLUGPLAY_REGKEY_DEVICE: u32 = 0x0000_0001;
const KEY_READ: u32 = 0x0002_0019;

// ---------------- 描述符 / 属性（每角色） ----------------

static HID_DESC_KBD: HID_DESCRIPTOR = HID_DESCRIPTOR {
    bLength: size_of::<HID_DESCRIPTOR>() as u8,
    bDescriptorType: 0x21,
    bcdHID: 0x0100,
    bCountryCode: 0,
    bNumDescriptors: 1,
    DescriptorList: [hid::HID_DESCRIPTOR_LIST_ENTRY {
        bDescriptorType: 0x22,
        wDescriptorLength: KEYBOARD_REPORT_DESCRIPTOR.len() as u16,
    }],
};

static HID_DESC_MOUSE: HID_DESCRIPTOR = HID_DESCRIPTOR {
    bLength: size_of::<HID_DESCRIPTOR>() as u8,
    bDescriptorType: 0x21,
    bcdHID: 0x0100,
    bCountryCode: 0,
    bNumDescriptors: 1,
    DescriptorList: [hid::HID_DESCRIPTOR_LIST_ENTRY {
        bDescriptorType: 0x22,
        wDescriptorLength: MOUSE_REPORT_DESCRIPTOR.len() as u16,
    }],
};

static HID_ATTR_KBD: HID_DEVICE_ATTRIBUTES = HID_DEVICE_ATTRIBUTES {
    Size: size_of::<HID_DEVICE_ATTRIBUTES>() as u32,
    VendorID: VID,
    ProductID: PID_KBD,
    VersionNumber: 0x0100,
};

static HID_ATTR_MOUSE: HID_DEVICE_ATTRIBUTES = HID_DEVICE_ATTRIBUTES {
    Size: size_of::<HID_DEVICE_ATTRIBUTES>() as u32,
    VendorID: VID,
    ProductID: PID_MOUSE,
    VersionNumber: 0x0100,
};

/// 设备角色
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Keyboard,
    Mouse,
}

fn report_descriptor(role: Role) -> &'static [u8] {
    match role {
        Role::Keyboard => &KEYBOARD_REPORT_DESCRIPTOR,
        Role::Mouse => &MOUSE_REPORT_DESCRIPTOR,
    }
}

fn hid_descriptor(role: Role) -> &'static HID_DESCRIPTOR {
    match role {
        Role::Keyboard => &HID_DESC_KBD,
        Role::Mouse => &HID_DESC_MOUSE,
    }
}

fn hid_attributes(role: Role) -> &'static HID_DEVICE_ATTRIBUTES {
    match role {
        Role::Keyboard => &HID_ATTR_KBD,
        Role::Mouse => &HID_ATTR_MOUSE,
    }
}

fn report_size(role: Role) -> usize {
    match role {
        Role::Keyboard => KBD_REPORT_SIZE,
        Role::Mouse => MOUSE_REPORT_SIZE,
    }
}

// ---------------- 设备状态（单锁保护双实例） ----------------

/// 单实例设备状态
struct Instance {
    /// 默认并行队列（用于请求→实例映射）
    default_queue: WDFQUEUE,
    /// manual 队列（挂起 READ_REPORT）
    manual_queue: WDFQUEUE,
    /// 最近一次注入的报告（键盘 8 字节 / 鼠标 4 字节，其余 0）
    report: [u8; 8],
    /// 本实例报告长度
    report_size: usize,
    /// 是否有已注入但尚未投递的报告
    report_ready: bool,
}

/// 全局状态：键盘 + 鼠标两个实例，同一把自旋锁
struct State {
    kbd: Instance,
    mouse: Instance,
}

struct StateCell {
    lock: AtomicBool,
    state: UnsafeCell<State>,
}

// SAFETY: state 仅在持有自旋锁时访问；WDFQUEUE 与 [u8; 8] 均可在内核线程间共享
unsafe impl Sync for StateCell {}

const fn new_instance() -> Instance {
    Instance {
        default_queue: core::ptr::null_mut(),
        manual_queue: core::ptr::null_mut(),
        report: [0; 8],
        report_size: 8,
        report_ready: false,
    }
}

static STATE: StateCell = StateCell {
    lock: AtomicBool::new(false),
    state: UnsafeCell::new(State {
        kbd: new_instance(),
        mouse: new_instance(),
    }),
};

/// 自旋锁守卫（RAII 释放）
struct StateGuard<'a>(&'a StateCell);

impl StateCell {
    fn lock(&self) -> StateGuard<'_> {
        while self
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        StateGuard(self)
    }
}

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
        self.0.lock.store(false, Ordering::Release);
    }
}

fn instance_mut(state: &mut State, role: Role) -> &mut Instance {
    match role {
        Role::Keyboard => &mut state.kbd,
        Role::Mouse => &mut state.mouse,
    }
}

/// 由默认队列句柄反查实例角色
fn role_for_queue(queue: WDFQUEUE) -> Option<Role> {
    let g = STATE.lock();
    if g.kbd.default_queue == queue {
        Some(Role::Keyboard)
    } else if g.mouse.default_queue == queue {
        Some(Role::Mouse)
    } else {
        None
    }
}

// ---------------- 设备添加 ----------------

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

/// 设备添加回调：创建设备 + 默认并行队列 + manual 队列，登记角色
///
/// # Safety
/// 由 WDF 调用，`driver`/`init` 为有效句柄。
unsafe extern "C" fn evt_device_add(_driver: WDFDRIVER, mut init: *mut WDFDEVICE_INIT) -> NTSTATUS {
    // 标记为过滤器（hidclass 之上；放弃电源策略所有权，与 vhidmini 一致）
    // SAFETY: init 有效
    unsafe {
        call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, init);
    }

    let mut attributes = wdk_sys::WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ..wdk_sys::WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: init/attributes/device 有效
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceCreate, &mut init, &mut attributes, &mut device)
    };
    if status < 0 {
        return status;
    }

    // 默认并行队列：接收 hidclass 的 IOCTL_HID_*
    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: wdk_sys::_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel,
        PowerManaged: 1, // WdfTrue
        AllowZeroLengthRequests: 1,
        DefaultQueue: 1,
        EvtIoInternalDeviceControl: Some(evt_io_internal_device_control),
        ..WDF_IO_QUEUE_CONFIG::default()
    };
    let mut queue_attr = wdk_sys::WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ..wdk_sys::WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut queue: WDFQUEUE = core::ptr::null_mut();
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &mut queue_config,
            &mut queue_attr,
            &mut queue
        )
    };
    if status < 0 {
        return status;
    }

    // manual 队列：挂起 READ_REPORT，等注入后由注入路径取回并完成
    let mut manual_config = WDF_IO_QUEUE_CONFIG {
        Size: size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: wdk_sys::_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchManual,
        PowerManaged: 0, // WdfFalse
        ..WDF_IO_QUEUE_CONFIG::default()
    };
    let mut manual_attr = wdk_sys::WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ..wdk_sys::WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut manual_queue: WDFQUEUE = core::ptr::null_mut();
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &mut manual_config,
            &mut manual_attr,
            &mut manual_queue
        )
    };
    if status < 0 {
        return status;
    }

    let role = read_role(device);
    {
        let mut g = STATE.lock();
        let inst = instance_mut(&mut g, role);
        inst.default_queue = queue;
        inst.manual_queue = manual_queue;
        inst.report_size = report_size(role);
    }
    STATUS_SUCCESS
}

// ---------------- IOCTL 分发 ----------------

/// 分发结果：Complete(status) 由调用方完成请求；Pending 表示请求已挂起（注入路径完成）
enum DispatchResult {
    Complete(NTSTATUS),
    Pending,
}

/// 内部设备控制：hidclass 的 IOCTL_HID_* 契约
///
/// # Safety
/// 由 WDF 队列分发，参数为有效句柄。
unsafe extern "C" fn evt_io_internal_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    output_len: usize,
    input_len: usize,
    ioctl_code: u32,
) {
    // SAFETY: request 有效
    let result = unsafe { dispatch_ioctl(queue, request, output_len, input_len, ioctl_code) };
    if let DispatchResult::Complete(status) = result {
        // SAFETY: 完成请求（WdfRequestComplete 保留 SetInformation 的字节数）
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestComplete, request, status);
        }
    }
}

/// 分发 HID IOCTL（按队列所属实例取角色）
///
/// # Safety
/// request 必须为有效 WDFREQUEST。
unsafe fn dispatch_ioctl(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_len: usize,
    _input_len: usize,
    code: u32,
) -> DispatchResult {
    let Some(role) = role_for_queue(queue) else {
        return DispatchResult::Complete(STATUS_INVALID_DEVICE_REQUEST);
    };
    match code {
        hid::IOCTL_HID_GET_DEVICE_DESCRIPTOR => {
            // SAFETY: 输出缓冲区由 hidclass 提供
            DispatchResult::Complete(unsafe { copy_to_output(request, hid_descriptor(role)) })
        }
        hid::IOCTL_HID_GET_DEVICE_ATTRIBUTES => {
            // SAFETY: 输出缓冲区由 hidclass 提供
            DispatchResult::Complete(unsafe { copy_to_output(request, hid_attributes(role)) })
        }
        hid::IOCTL_HID_GET_REPORT_DESCRIPTOR => {
            // SAFETY: 输出缓冲区由 hidclass 提供
            DispatchResult::Complete(unsafe { copy_to_output(request, report_descriptor(role)) })
        }
        hid::IOCTL_HID_READ_REPORT => {
            // 输入报告：已有注入则立即投递，否则挂 manual 队列等注入
            unsafe { handle_read_report(role, request) }
        }
        hid::IOCTL_HID_WRITE_REPORT | hid::IOCTL_HID_SET_OUTPUT_REPORT => {
            // 注入入口：接收报告并投递
            DispatchResult::Complete(unsafe { handle_inject(role, request) })
        }
        hid::IOCTL_HID_GET_INPUT_REPORT => {
            // HidD_GetInputReport：返回当前状态
            DispatchResult::Complete(unsafe { handle_get_input_report(role, request) })
        }
        hid::IOCTL_HID_GET_FEATURE | hid::IOCTL_HID_SET_FEATURE => {
            // 无 feature 报告；空操作
            DispatchResult::Complete(STATUS_SUCCESS)
        }
        hid::IOCTL_HID_GET_STRING => {
            // 设备名由 INF 提供；返回空串即可
            DispatchResult::Complete(STATUS_SUCCESS)
        }
        _ => DispatchResult::Complete(STATUS_INVALID_DEVICE_REQUEST),
    }
}

/// IOCTL_HID_READ_REPORT：投递一个输入报告
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn handle_read_report(role: Role, request: WDFREQUEST) -> DispatchResult {
    let mut report = [0u8; 8];
    let mut size = 0usize;
    let manual_queue: WDFQUEUE;
    let mut have = false;
    {
        let mut g = STATE.lock();
        let inst = instance_mut(&mut g, role);
        if inst.report_ready {
            inst.report_ready = false;
            report = inst.report;
            size = inst.report_size;
            have = true;
        }
        manual_queue = inst.manual_queue;
    }
    if have {
        // SAFETY: 输出缓冲区由 hidclass 提供
        return DispatchResult::Complete(unsafe { copy_to_output(request, &report[..size]) });
    }
    // 否则挂到 manual 队列等待注入
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestForwardToIoQueue, request, manual_queue)
    };
    if status < 0 {
        return DispatchResult::Complete(status);
    }
    // 竞态收尾：转发期间恰好有注入到达时，manual 队列至多一个挂起请求，
    // 取回（就是本请求）并立即投递，避免永久挂起。
    let mut req: WDFREQUEST = core::ptr::null_mut();
    let mut ready = false;
    {
        let mut g = STATE.lock();
        let inst = instance_mut(&mut g, role);
        if inst.report_ready {
            let status = unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfIoQueueRetrieveNextRequest,
                    inst.manual_queue,
                    &mut req
                )
            };
            if status >= 0 && !req.is_null() {
                inst.report_ready = false;
                report = inst.report;
                size = inst.report_size;
                ready = true;
            }
        }
    }
    if ready {
        // 取回的就是本请求（hidclass 同一时刻至多一个挂起读）；自行完成
        // SAFETY: req 有效，输出缓冲区由 hidclass 提供
        let status = unsafe { copy_to_output(req, &report[..size]) };
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestComplete, req, status);
        }
        return DispatchResult::Pending;
    }
    DispatchResult::Pending
}

/// 注入入口：解析 HID_XFER_PACKET，把报告写入状态并投递
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn handle_inject(role: Role, request: WDFREQUEST) -> NTSTATUS {
    let report_size = report_size(role);
    let mut packet = HID_XFER_PACKET::default();
    let status = unsafe { retrieve_packet(request, true, &mut packet) };
    if status < 0 {
        return status;
    }
    if packet.reportBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if (packet.reportBufferLen as usize) < report_size {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    let mut report = [0u8; 8];
    // SAFETY: reportBuffer 指向 hidclass 提供的已锁定内核缓冲区，长度 >= report_size
    unsafe {
        core::ptr::copy_nonoverlapping(
            packet.reportBuffer.cast::<u8>(),
            report.as_mut_ptr(),
            report_size,
        );
    }
    inject_report(role, &report[..report_size]);
    // SAFETY: 记录已消费字节数
    unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestSetInformation, request, report_size as u64);
    }
    STATUS_SUCCESS
}

/// HidD_GetInputReport：把当前报告拷贝到 packet.reportBuffer
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn handle_get_input_report(role: Role, request: WDFREQUEST) -> NTSTATUS {
    let report_size = report_size(role);
    let mut packet = HID_XFER_PACKET::default();
    let status = unsafe { retrieve_packet(request, false, &mut packet) };
    if status < 0 {
        return status;
    }
    if packet.reportBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if (packet.reportBufferLen as usize) < report_size {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    let report = {
        let g = STATE.lock();
        match role {
            Role::Keyboard => g.kbd.report,
            Role::Mouse => g.mouse.report,
        }
    };
    // SAFETY: reportBuffer 指向 hidclass 提供的已锁定输出缓冲区，长度 >= report_size
    unsafe {
        core::ptr::copy_nonoverlapping(
            report.as_ptr(),
            packet.reportBuffer.cast::<u8>(),
            report_size,
        );
    }
    // SAFETY: 记录写入字节数
    unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestSetInformation, request, report_size as u64);
    }
    STATUS_SUCCESS
}

/// 把注入报告写入状态，并投递给挂起的 READ_REPORT（若有）
fn inject_report(role: Role, report: &[u8]) {
    let mut req: WDFREQUEST = core::ptr::null_mut();
    {
        let mut g = STATE.lock();
        let inst = instance_mut(&mut g, role);
        inst.report[..report.len()].copy_from_slice(report);
        inst.report_ready = true;
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                inst.manual_queue,
                &mut req
            )
        };
        if status >= 0 && !req.is_null() {
            inst.report_ready = false;
        }
    }
    if !req.is_null() {
        // 完成挂起的 READ_REPORT；hidclass 同一时刻至多一个挂起读
        // SAFETY: req 有效
        let status = unsafe { copy_to_output(req, report) };
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestComplete, req, status);
        }
    }
}

// ---------------- 缓冲区辅助 ----------------

/// 从请求输入/输出缓冲区取 HID_XFER_PACKET
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn retrieve_packet(
    request: WDFREQUEST,
    from_input: bool,
    packet: &mut HID_XFER_PACKET,
) -> NTSTATUS {
    let mut memory: WDFMEMORY = core::ptr::null_mut();
    let status = if from_input {
        // SAFETY: 取输入缓冲区
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestRetrieveInputMemory, request, &mut memory)
        }
    } else {
        // SAFETY: 取输出缓冲区
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestRetrieveOutputMemory, request, &mut memory)
        }
    };
    if status < 0 {
        return status;
    }
    let mut len: usize = 0;
    // SAFETY: memory 有效
    let buf: PVOID =
        unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &mut len) };
    if len < size_of::<HID_XFER_PACKET>() {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    // SAFETY: buf 指向 hidclass 传入的 HID_XFER_PACKET，长度 >= sizeof
    unsafe {
        core::ptr::copy_nonoverlapping(
            buf.cast::<u8>(),
            (packet as *mut HID_XFER_PACKET).cast::<u8>(),
            size_of::<HID_XFER_PACKET>(),
        );
    }
    STATUS_SUCCESS
}

/// 把固定数据拷到请求输出缓冲区（结构体或字节切片均可），并记录信息字节数
///
/// # Safety
/// request 有效；data 指向有效内存。
unsafe fn copy_to_output<T: ?Sized>(request: WDFREQUEST, data: &T) -> NTSTATUS {
    // SAFETY: data 有效；按字节视图读出，不修改
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (data as *const T).cast::<u8>(),
            core::mem::size_of_val(data),
        )
    };
    let mut memory: WDFMEMORY = core::ptr::null_mut();
    // SAFETY: 取输出内存
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestRetrieveOutputMemory, request, &mut memory)
    };
    if status < 0 {
        return status;
    }
    let mut out_len: usize = 0;
    // SAFETY: memory 有效
    let buf: PVOID =
        unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &mut out_len) };
    if out_len < bytes.len() {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    // SAFETY: buf 指向输出缓冲区且长度 >= bytes.len()
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), bytes.len());
    }
    // SAFETY: 记录已写字节数
    unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestSetInformation, request, bytes.len() as u64);
    }
    STATUS_SUCCESS
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
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver as PDRIVER_OBJECT,
            registry_path,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut config,
            WDF_NO_HANDLE.cast::<WDFDRIVER>(),
        )
    }
}

/// panic 处理器：内核不允许 unwind，卡死等待调试器
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
