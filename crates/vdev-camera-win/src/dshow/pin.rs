//! `OutputPin`：捕获源过滤器的输出 Pin。
//!
//! 实现 `IPin`（连接协商 / 枚举 / 流控制）与 `IAMStreamConfig`（格式能力，
//! ffmpeg 等消费方依赖它查询/设置输出格式）。

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use windows::Win32::Foundation::{E_POINTER, S_FALSE, S_OK};
use windows::Win32::Media::DirectShow::{
    IAMStreamConfig, IAMStreamConfig_Impl, IEnumMediaTypes, IMemAllocator, IMemInputPin, IPin,
    IPin_Impl, ALLOCATOR_PROPERTIES, AMPROPERTY_PIN_CATEGORY, PINDIR_OUTPUT, PIN_DIRECTION,
    PIN_INFO, VFW_E_ALREADY_CONNECTED, VFW_E_CANNOT_CONNECT, VFW_E_INVALIDMEDIATYPE,
    VFW_E_NOT_CONNECTED, VFW_E_TYPE_NOT_ACCEPTED, VFW_E_WRONG_STATE, VIDEO_STREAM_CONFIG_CAPS,
};
use windows::Win32::Media::KernelStreaming::{IKsPropertySet, IKsPropertySet_Impl};
use windows::Win32::Media::MediaFoundation::{
    AMPROPSETID_Pin, CLSID_MemoryAllocator, AM_MEDIA_TYPE, PIN_CATEGORY_CAPTURE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows_core::{implement, Error, Interface, OutRef, Ref, GUID, HRESULT, PWSTR};

use super::enum_media_types::MediaTypeEnum;
use super::filter::FilterInner;
use super::media_type::{self, media_type_matches, VideoFormat, FORMATS};
use super::util::alloc_pwstr;

/// 输出 Pin 名（`QueryId` / `PIN_INFO.achName`）。
pub const PIN_NAME: &str = "Capture";

/// 连接状态快照（推流线程使用；COM 接口句柄克隆开销极小）。
///
/// SAFETY 说明：DirectShow 过滤器运行在 MTA 下，下游接口（IPin/IMemInputPin/
/// IMemAllocator）允许跨线程调用；快照仅由推流线程使用，发送到该线程安全。
#[derive(Clone)]
pub struct ConnectedState {
    pub peer: IPin,
    pub input: IMemInputPin,
    pub allocator: IMemAllocator,
    pub format: VideoFormat,
}

// SAFETY: 见 ConnectedState 说明——MTA 下 COM 接口可跨线程；本快照仅由推流线程使用。
unsafe impl Send for ConnectedState {}

// SAFETY: PinInner 内部所有共享状态均受 Mutex/Atomic 保护；包含的 COM 接口
// （自身 IPin、下游连接）在 DirectShow MTA 语义下允许跨线程访问。
unsafe impl Send for PinInner {}
unsafe impl Sync for PinInner {}

/// 输出 Pin 共享状态。
pub struct PinInner {
    /// 所属过滤器（Weak，避免循环强引用）。
    pub filter: Mutex<Option<Weak<FilterInner>>>,
    pub name: Vec<u16>,
    current: Mutex<usize>,
    connected: Mutex<Option<ConnectedState>>,
    /// 自身 `IPin`（构造时缓存，供 `Connect` 协商与枚举返回同一对象）。
    self_pin: Mutex<Option<IPin>>,
    /// 下游是否处于 Flush 状态。
    pub flushing: AtomicBool,
    /// 最近一次 NewSegment 参数。
    pub segment: Mutex<(i64, i64, f64)>,
}

impl PinInner {
    pub fn unattached() -> Self {
        Self {
            filter: Mutex::new(None),
            name: PIN_NAME.encode_utf16().collect(),
            current: Mutex::new(0),
            connected: Mutex::new(None),
            self_pin: Mutex::new(None),
            flushing: AtomicBool::new(false),
            segment: Mutex::new((0, 0, 1.0)),
        }
    }

    pub fn self_pin(&self) -> windows_core::Result<IPin> {
        self.self_pin
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::from_hresult(VFW_E_NOT_CONNECTED))
    }

    /// 缓存自身 `IPin`（构造时由过滤器调用一次）。
    pub(crate) fn set_self_pin(&self, pin: IPin) {
        *self.self_pin.lock().unwrap() = Some(pin);
    }

    pub fn is_connected(&self) -> bool {
        self.connected.lock().unwrap().is_some()
    }

    pub fn connected(&self) -> Option<ConnectedState> {
        self.connected.lock().unwrap().clone()
    }

    pub fn current_format(&self) -> VideoFormat {
        let idx = *self.current.lock().unwrap();
        FORMATS[idx.min(FORMATS.len() - 1)]
    }

    /// 推流线程每交付一帧调用（自测计数）。
    pub fn note_frame(&self) {
        if let Some(f) = self.filter.lock().unwrap().as_ref().and_then(Weak::upgrade) {
            f.frames_delivered.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 输出 Pin COM 对象（单一对象，QI 分发给 `IPin` / `IAMStreamConfig`）。
#[implement(IPin, IAMStreamConfig, IKsPropertySet)]
pub struct OutputPin {
    pub inner: Arc<PinInner>,
}

/// 校验媒体类型并转换为 [`VideoFormat`]（仅接受我们支持的 RGB32 格式）。
fn accepted_format(pmt: *const AM_MEDIA_TYPE) -> Option<VideoFormat> {
    if pmt.is_null() {
        return None;
    }
    // SAFETY: pmt 由调用方保证指向有效 AM_MEDIA_TYPE。
    media_type_matches(unsafe { &*pmt })
}

impl IPin_Impl for OutputPin_Impl {
    fn Connect(
        &self,
        preceivepin: Ref<IPin>,
        pmt: *const AM_MEDIA_TYPE,
    ) -> windows_core::Result<()> {
        let peer = preceivepin.ok()?;
        if self.inner.is_connected() {
            return Err(Error::from_hresult(VFW_E_ALREADY_CONNECTED));
        }
        let self_pin = self.inner.self_pin()?;

        let format = if pmt.is_null() {
            let mut chosen = None;
            for f in FORMATS {
                let mt = f.to_media_type();
                // SAFETY: 传递栈上 AM_MEDIA_TYPE 指针；失败后释放 pbFormat。
                let hr = unsafe { peer.ReceiveConnection(&self_pin, &mt) };
                media_type::free_format(&mt);
                if hr.is_ok() {
                    chosen = Some(f);
                    break;
                }
            }
            chosen.ok_or_else(|| Error::from_hresult(VFW_E_CANNOT_CONNECT))?
        } else {
            let f =
                accepted_format(pmt).ok_or_else(|| Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED))?;
            // SAFETY: pmt 由调用方保证有效。
            match unsafe { peer.ReceiveConnection(&self_pin, pmt) } {
                Ok(()) => {}
                Err(e) => {
                    return Err(e);
                }
            }
            f
        };

        // 2. 取下游 IMemInputPin（推源过滤器需要 IMemInputPin 接收数据）。
        let input: IMemInputPin = match peer.cast() {
            Ok(i) => i,
            Err(_) => {
                // 下游不支持内存输入 → 释放已协商连接（尽力而为）。
                // SAFETY: peer 有效；Disconnect 撤销尚未正式提交的连接。
                let _ = unsafe { peer.Disconnect() };
                return Err(Error::from_hresult(VFW_E_CANNOT_CONNECT));
            }
        };

        // 3. allocator negotiation (DirectShow push-source protocol, mirroring CBaseOutputPin::DecideAllocator):
        //    prefer the downstream-provided allocator; if the downstream provides none (VFW_E_NO_ALLOCATOR)
        //    or rejects our properties, create a CLSID_MemoryAllocator and notify the downstream.
        let props = ALLOCATOR_PROPERTIES {
            cBuffers: 3,
            cbBuffer: format.frame_size() as i32,
            cbAlign: 1,
            cbPrefix: 0,
        };
        let allocator = decide_allocator(&input, &props)?;

        // 4. 提交连接状态。
        let mut guard = self.inner.connected.lock().unwrap();
        if guard.is_some() {
            return Err(Error::from_hresult(VFW_E_ALREADY_CONNECTED));
        }
        *guard = Some(ConnectedState {
            peer: peer.clone(),
            input,
            allocator,
            format,
        });
        Ok(())
    }

    fn ReceiveConnection(
        &self,
        _pconnector: Ref<IPin>,
        _pmt: *const AM_MEDIA_TYPE,
    ) -> windows_core::Result<()> {
        // 输出 Pin 不接收连接。
        Err(Error::from_hresult(
            windows::Win32::Media::DirectShow::VFW_E_TYPE_NOT_ACCEPTED,
        ))
    }

    fn Disconnect(&self) -> windows_core::Result<()> {
        let mut guard = self.inner.connected.lock().unwrap();
        if guard.is_none() {
            return Err(Error::from_hresult(VFW_E_NOT_CONNECTED));
        }
        *guard = None;
        Ok(())
    }

    fn ConnectedTo(&self) -> windows_core::Result<IPin> {
        self.inner
            .connected()
            .map(|c| c.peer)
            .ok_or_else(|| Error::from_hresult(VFW_E_NOT_CONNECTED))
    }

    fn ConnectionMediaType(&self, pmt: *mut AM_MEDIA_TYPE) -> windows_core::Result<()> {
        if pmt.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let conn = self
            .inner
            .connected()
            .ok_or_else(|| Error::from_hresult(VFW_E_NOT_CONNECTED))?;
        // SAFETY: pmt 由调用方分配；pbFormat 所有权转移给调用方。
        unsafe { *pmt = conn.format.to_media_type() };
        Ok(())
    }

    fn QueryPinInfo(&self, pinfo: *mut PIN_INFO) -> windows_core::Result<()> {
        if pinfo.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: pinfo 由调用方分配。
        let info = unsafe { &mut *pinfo };
        let base = self
            .inner
            .filter
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
            .and_then(|f| f.self_base_owned());
        // pFilter 所有权交给调用方（调用方负责 Release）。
        info.pFilter = ManuallyDrop::new(base);
        info.dir = PINDIR_OUTPUT;
        super::filter::write_ach_name(&mut info.achName, &self.inner.name);
        Ok(())
    }

    fn QueryDirection(&self) -> windows_core::Result<PIN_DIRECTION> {
        Ok(PINDIR_OUTPUT)
    }

    fn QueryId(&self) -> windows_core::Result<PWSTR> {
        // SAFETY: 返回 CoTaskMem 字符串，调用方释放。
        Ok(unsafe { alloc_pwstr(PIN_NAME) })
    }

    fn QueryAccept(&self, pmt: *const AM_MEDIA_TYPE) -> HRESULT {
        match accepted_format(pmt) {
            Some(_) => S_OK,
            None => S_FALSE,
        }
    }

    fn EnumMediaTypes(&self) -> windows_core::Result<IEnumMediaTypes> {
        Ok(MediaTypeEnum::new(FORMATS.to_vec()).into())
    }

    fn QueryInternalConnections(
        &self,
        _appin: OutRef<IPin>,
        npin: *mut u32,
    ) -> windows_core::Result<()> {
        if !npin.is_null() {
            // SAFETY: npin 由调用方分配。
            unsafe { *npin = 0 };
        }
        Ok(())
    }

    fn EndOfStream(&self) -> windows_core::Result<()> {
        // 推源过滤器没有上游；收到 EndOfStream 视为空操作。
        Ok(())
    }

    fn BeginFlush(&self) -> windows_core::Result<()> {
        self.inner.flushing.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn EndFlush(&self) -> windows_core::Result<()> {
        self.inner.flushing.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn NewSegment(&self, tstart: i64, tstop: i64, drate: f64) -> windows_core::Result<()> {
        *self.inner.segment.lock().unwrap() = (tstart, tstop, drate);
        Ok(())
    }
}

impl IAMStreamConfig_Impl for OutputPin_Impl {
    fn SetFormat(&self, pmt: *const AM_MEDIA_TYPE) -> windows_core::Result<()> {
        let f = accepted_format(pmt).ok_or_else(|| Error::from_hresult(VFW_E_INVALIDMEDIATYPE))?;
        if self.inner.is_connected() {
            // 连接后再改格式需重启流；v1 仅允许在未连接/未运行时设置。
            return Err(Error::from_hresult(VFW_E_WRONG_STATE));
        }
        let idx = FORMATS
            .iter()
            .position(|x| *x == f)
            .ok_or_else(|| Error::from_hresult(VFW_E_INVALIDMEDIATYPE))?;
        *self.inner.current.lock().unwrap() = idx;
        Ok(())
    }

    fn GetFormat(&self) -> windows_core::Result<*mut AM_MEDIA_TYPE> {
        let f = self.inner.current_format();
        let mt = f.to_media_type();
        // SAFETY: 返回 CoTaskMem 拷贝，调用方用 DeleteMediaType 释放。
        Ok(unsafe { media_type::alloc_media_type_copy(&mt) })
    }

    fn GetNumberOfCapabilities(
        &self,
        picount: *mut i32,
        pisize: *mut i32,
    ) -> windows_core::Result<()> {
        if picount.is_null() || pisize.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: 指针由调用方分配。
        unsafe {
            *picount = FORMATS.len() as i32;
            *pisize = std::mem::size_of::<VIDEO_STREAM_CONFIG_CAPS>() as i32;
        }
        Ok(())
    }

    fn GetStreamCaps(
        &self,
        iindex: i32,
        ppmt: *mut *mut AM_MEDIA_TYPE,
        pscc: *mut u8,
    ) -> windows_core::Result<()> {
        if ppmt.is_null() || pscc.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let idx = iindex as usize;
        let f = FORMATS
            .get(idx)
            .ok_or_else(|| Error::from_hresult(VFW_E_INVALIDMEDIATYPE))?;
        let mt = f.to_media_type();
        // SAFETY: ppmt 由调用方分配；返回 CoTaskMem 拷贝。
        unsafe {
            *ppmt = media_type::alloc_media_type_copy(&mt);
            let caps = video_stream_config_caps(f);
            std::ptr::copy_nonoverlapping(
                (&caps as *const VIDEO_STREAM_CONFIG_CAPS).cast::<u8>(),
                pscc,
                std::mem::size_of::<VIDEO_STREAM_CONFIG_CAPS>(),
            );
        }
        Ok(())
    }
}

/// 构造 `VIDEO_STREAM_CONFIG_CAPS`（固定单档分辨率/帧率）。
/// `IKsPropertySet`：向消费方（如 ffmpeg 的 dshow 输入）声明本 Pin 是
/// 「捕获」（Capture）类别输出 Pin。缺此接口时 ffmpeg 会直接跳过该 Pin
/// （dshow_cycle_pins 要求 QI 成功且返回 PIN_CATEGORY_CAPTURE）。
impl IKsPropertySet_Impl for OutputPin_Impl {
    fn Get(
        &self,
        guidpropset: *const windows_core::GUID,
        dwpropid: u32,
        _pinstancedata: *const core::ffi::c_void,
        _cbinstancedata: u32,
        ppropdata: *mut core::ffi::c_void,
        cbpropdata: u32,
        pcbreturned: *mut u32,
    ) -> windows_core::Result<()> {
        if guidpropset.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: guidpropset 由调用方保证指向有效 GUID。
        let set = unsafe { &*guidpropset };
        if *set == AMPROPSETID_Pin && dwpropid == AMPROPERTY_PIN_CATEGORY.0 as u32 {
            if ppropdata.is_null() || pcbreturned.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            if cbpropdata < std::mem::size_of::<GUID>() as u32 {
                return Err(Error::from_hresult(
                    HRESULT::from_win32(0x7A), // ERROR_INSUFFICIENT_BUFFER
                ));
            }
            // SAFETY: ppropdata 由调用方分配且容量 >= GUID；pcbreturned 有效。
            unsafe {
                std::ptr::write(ppropdata.cast::<GUID>(), PIN_CATEGORY_CAPTURE);
                std::ptr::write(pcbreturned, std::mem::size_of::<GUID>() as u32);
            }
            Ok(())
        } else {
            Err(Error::from_hresult(windows::Win32::Foundation::E_NOTIMPL))
        }
    }

    fn Set(
        &self,
        _guidpropset: *const windows_core::GUID,
        _dwpropid: u32,
        _pinstancedata: *const core::ffi::c_void,
        _cbinstancedata: u32,
        _ppropdata: *const core::ffi::c_void,
        _cbpropdata: u32,
    ) -> windows_core::Result<()> {
        // 只读属性集：Pin 类别不可由外部修改。
        Err(Error::from_hresult(windows::Win32::Foundation::E_NOTIMPL))
    }

    fn QuerySupported(
        &self,
        guidpropset: *const windows_core::GUID,
        dwpropid: u32,
    ) -> windows_core::Result<u32> {
        if guidpropset.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: guidpropset 由调用方保证指向有效 GUID。
        let set = unsafe { &*guidpropset };
        if *set == AMPROPSETID_Pin && dwpropid == AMPROPERTY_PIN_CATEGORY.0 as u32 {
            Ok(1) // KSPROPERTY_SUPPORT_GET
        } else {
            Err(Error::from_hresult(windows::Win32::Foundation::E_NOTIMPL))
        }
    }
}

fn video_stream_config_caps(f: &VideoFormat) -> VIDEO_STREAM_CONFIG_CAPS {
    use windows::Win32::Foundation::SIZE;
    let size = SIZE {
        cx: f.width as i32,
        cy: f.height as i32,
    };
    let interval = 10_000_000 / f.fps as i64;
    VIDEO_STREAM_CONFIG_CAPS {
        guid: windows::Win32::Media::MediaFoundation::FORMAT_VideoInfo,
        VideoStandard: 0,
        InputSize: size,
        MinCroppingSize: size,
        MaxCroppingSize: size,
        CropGranularityX: 0,
        CropGranularityY: 0,
        CropAlignX: 0,
        CropAlignY: 0,
        MinOutputSize: size,
        MaxOutputSize: size,
        OutputGranularityX: 0,
        OutputGranularityY: 0,
        StretchTapsX: 0,
        StretchTapsY: 0,
        ShrinkTapsX: 0,
        ShrinkTapsY: 0,
        MinFrameInterval: interval,
        MaxFrameInterval: interval,
        MinBitsPerSecond: (f.width * f.height * 32 * f.fps) as i32,
        MaxBitsPerSecond: (f.width * f.height * 32 * f.fps) as i32,
    }
}

/// 分配器协商（DirectShow 推源协议，镜像 `CBaseOutputPin::DecideAllocator`）。
///
/// 1. 优先使用下游 `IMemInputPin::GetAllocator` 提供的分配器；其属性协商与
///    `NotifyAllocator` 都成功则直接采用。
/// 2. 下游不提供（`VFW_E_NO_ALLOCATOR`，如 ffmpeg 的 dshow sink）或属性被拒
///    （如 `E_INVALIDARG`）时，自建标准 `CLSID_MemoryAllocator` 并通知下游。
///    推源过滤器必须自己提供分配器，不能要求下游分配。
fn decide_allocator(
    input: &IMemInputPin,
    props: &ALLOCATOR_PROPERTIES,
) -> windows_core::Result<IMemAllocator> {
    // 先试下游提供的分配器。
    // SAFETY: COM 调用，GetAllocator 失败返回错误。
    if let Ok(alloc) = unsafe { input.GetAllocator() } {
        // SAFETY: props/actual 为有效指针。
        if unsafe { alloc.SetProperties(props) }.is_ok()
            && unsafe { input.NotifyAllocator(&alloc, false) }.is_ok()
        {
            return Ok(alloc);
        }
    }
    // 自建标准内存分配器。
    // SAFETY: CoCreateInstance(CLSID_MemoryAllocator) 返回有效 IMemAllocator。
    let own: IMemAllocator = unsafe { CoCreateInstance(&CLSID_MemoryAllocator, None, CLSCTX_ALL) }?;
    // SAFETY: props 为有效指针。
    unsafe { own.SetProperties(props) }?;
    // SAFETY: Ref 借用 own。
    unsafe { input.NotifyAllocator(&own, false) }?;
    Ok(own)
}
