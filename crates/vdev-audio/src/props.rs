// 属性实现：plug-in / box / device / stream / control 的 Has/Settable/Size/Get/Set。
// 属性选择器用标准四字符值（macOS 26 SDK 头文件移除了部分常量名，值不变）。

// ---- 通用对象属性（macOS 26 SDK：manufacturer='lmak', name='lnam', model='lmod'）----
#[repr(C)]
#[derive(Clone, Copy)]
struct CustomPropertyInfo {
    m_selector: u32,
    m_data_type: u32,        // kAudioServerPlugInCustomPropertyDataTypeNone = 0
    m_qualifier_data_type: u32, // 无 qualifier
}

const SEL_LNAM: u32 = 0x6c6e_616d; // kAudioObjectPropertyName 'lnam' 
const SEL_LMOD: u32 = 0x6c6d_6f64; // kAudioObjectPropertyModelName 'lmod'
const SEL_LMAK: u32 = 0x6c6d_616b; // kAudioObjectPropertyManufacturer 'lmak'
const SEL_CLAS: u32 = 0x636c_6173; // kAudioObjectPropertyClass 'clas'
const SEL_BCLS: u32 = 0x6263_6c73; // kAudioObjectPropertyBaseClass 'bcls'
const SEL_OWNE: u32 = 0x7374_6476; // kAudioObjectPropertyOwner 'stdv' (macOS 26)
const SEL_OWND: u32 = 0x6f77_6e64; // kAudioObjectPropertyOwnedObjects 'ownd'
const SEL_RING: u32 = 0x7269_6e67; // kAudioDevicePropertyZeroTimeStampPeriod 'ring'
const SEL_CSCP: u32 = 0x6373_6370; // kAudioControlPropertyScope 'cscp'
const SEL_CELM: u32 = 0x6365_6c6d; // kAudioControlPropertyElement 'celm'
const SEL_CSTB: u32 = 0x6373_7462; // kAudioDevicePropertyClockIsStable 'cstb'

// ---- plug-in ----
const SEL_PMFR: u32 = 0x706d_6672; // kAudioPlugInPropertyManufacturer 'pmfr'
const SEL_PNAM: u32 = 0x706e_616d; // kAudioPlugInPropertyName 'pnam'
const SEL_PVER: u32 = 0x7076_6572; // kAudioPlugInPropertyVersion 'pver'
const SEL_RSRC: u32 = 0x7273_7263; // kAudioPlugInPropertyResourceBundle 'rsrc'
const SEL_PBOX: u32 = 0x626f_7823; // kAudioPlugInPropertyBoxList 'box#'
const SEL_PDEV: u32 = 0x6465_7623; // kAudioPlugInPropertyDeviceList 'dev#'
const SEL_UIDB: u32 = 0x7569_6462; // TranslateUIDToBox 'uidb'
const SEL_UIDD: u32 = 0x7569_6464; // TranslateUIDToDevice 'uidd'
// ---- box ----
const SEL_BUID: u32 = 0x6275_6964; // kAudioBoxPropertyBoxUID 'buid'
const SEL_BTRN: u32 = 0x7472_616e; // kAudioBoxPropertyTransportType 'tran'
const SEL_BHAU: u32 = 0x6268_6175; // kAudioBoxPropertyHasAudio 'bhau'
const SEL_BHVI: u32 = 0x6268_7669; // kAudioBoxPropertyHasVideo 'bhvi'
const SEL_BHMI: u32 = 0x6268_6d69; // kAudioBoxPropertyHasMIDI 'bhmi'
const SEL_BPRO: u32 = 0x6270_726f; // kAudioBoxPropertyIsProtected 'bpro'
const SEL_BXON: u32 = 0x6278_6f6e; // kAudioBoxPropertyAcquired 'bxon'
const SEL_BXOF: u32 = 0x6278_6f66; // kAudioBoxPropertyAcquisitionFailed 'bxof'
const SEL_BDV: u32 = 0x6264_7623; // kAudioBoxPropertyDeviceList 'bdv#'
const SEL_BNAM: u32 = 0x626e_616d; // kAudioBoxPropertyName 'bnam'
const SEL_BMFR: u32 = 0x626d_6672; // kAudioBoxPropertyManufacturer 'bmfr'
const SEL_BMOD: u32 = 0x626d_6f64; // kAudioBoxPropertyModel 'bmod'
const SEL_BSNO: u32 = 0x6273_6e6f; // kAudioBoxPropertySerialNumber 'bsno'
const SEL_BFMW: u32 = 0x6266_6d77; // kAudioBoxPropertyFirmwareVersion 'bfmw'
const SEL_IDEN: u32 = 0x6964_656e; // kAudioObjectPropertyIdentify 'iden'
const SEL_SNUM: u32 = 0x736e_756d; // kAudioObjectPropertySerialNumber 'snum'
const SEL_FWVN: u32 = 0x6677_766e; // kAudioObjectPropertyFirmwareVersion 'fwvn'
// ---- device ----
const SEL_UID: u32 = 0x7569_6420; // kAudioDevicePropertyDeviceUID 'uid '
const SEL_MUID: u32 = 0x6d75_6964; // kAudioDevicePropertyModelUID 'muid'
const SEL_ICON: u32 = 0x6963_6f6e; // kAudioDevicePropertyIcon 'icon'
const SEL_SRND: u32 = 0x7372_6e64; // kAudioDevicePropertyPreferredChannelLayout 'srnd'
const SEL_TRAN: u32 = 0x7472_616e; // kAudioDevicePropertyTransportType 'tran'
const SEL_GROU: u32 = 0x6772_6f75; // kAudioDevicePropertyRelatedDevices 'grou'
const SEL_CLKD: u32 = 0x636c_6b64; // kAudioDevicePropertyClockDomain 'clkd'
const SEL_CLOK: u32 = 0x636c_6f6b; // kAudioDevicePropertyClockAlgorithm 'clok' (macOS 26)
const SEL_LIVN: u32 = 0x6c69_766e; // kAudioDevicePropertyDeviceIsAlive 'livn'
const SEL_GOIN: u32 = 0x676f_696e; // kAudioDevicePropertyDeviceIsRunning 'goin'
const SEL_GONE: u32 = 0x676f_6e65; // kAudioDevicePropertyDeviceIsRunningSomewhere 'gone'
const SEL_DFLT: u32 = 0x6466_6c74; // kAudioDevicePropertyDeviceCanBeDefaultDevice 'dflt'
const SEL_SFLT: u32 = 0x7366_6c74; // kAudioDevicePropertyDeviceCanBeDefaultSystemDevice 'sflt'
const SEL_LTNC: u32 = 0x6c74_6e63; // kAudioDevicePropertyLatency 'ltnc'
const SEL_STM: u32 = 0x7374_6d23; // kAudioDevicePropertyStreams 'stm#'
const SEL_CTRL: u32 = 0x6374_726c; // kAudioDevicePropertyControlList 'ctrl'
const SEL_SAFT: u32 = 0x7361_6674; // kAudioDevicePropertySafetyOffset 'saft'
const SEL_NSRT: u32 = 0x6e73_7274; // kAudioDevicePropertyNominalSampleRate 'nsrt'
const SEL_NSR: u32 = 0x6e73_7223; // kAudioDevicePropertyAvailableNominalSampleRates 'nsr#'
const SEL_HIDN: u32 = 0x6869_646e; // kAudioDevicePropertyIsHidden 'hidn'
const SEL_FSIZ: u32 = 0x6673_697a; // kAudioDevicePropertyBufferFrameSize 'fsiz'
const SEL_FSZ: u32 = 0x6673_7a23; // kAudioDevicePropertyBufferFrameSizeRange 'fsz#'
const SEL_VFSZ: u32 = 0x7666_737a; // kAudioDevicePropertyUsesVariableBufferFrameSizes 'vfsz'
const SEL_DCH2: u32 = 0x6463_6832; // kAudioDevicePropertyPreferredChannelsForStereo 'dch2'
// ---- stream ----
const SEL_SACT: u32 = 0x7361_6374; // kAudioStreamPropertyIsActive 'sact'
const SEL_SDIR: u32 = 0x7364_6972; // kAudioStreamPropertyDirection 'sdir'
const SEL_TERM: u32 = 0x7465_726d; // kAudioStreamPropertyTerminalType 'term'
const SEL_SCHN: u32 = 0x7363_686e; // kAudioStreamPropertyStartingChannel 'schn'
const SEL_SFMT: u32 = 0x7366_6d74; // kAudioStreamPropertyVirtualFormat 'sfmt'
const SEL_PFT: u32 = 0x7066_7420; // kAudioStreamPropertyPhysicalFormat 'pft '
const SEL_SFMA: u32 = 0x7366_6d61; // kAudioStreamPropertyAvailableVirtualFormats 'sfma'
const SEL_PFTA: u32 = 0x7066_7461; // kAudioStreamPropertyAvailablePhysicalFormats 'pfta'
// ---- control ----
const SEL_STBL: u32 = 0x7374_626c; // kAudioControlPropertyIsSettable 'stbl'
const SEL_VLSC: u32 = 0x766c_7363; // kAudioVolumeControlPropertyScalarValue 'vlsc'
const SEL_VMIN: u32 = 0x766d_696e; // kAudioVolumeControlPropertyMinimumScalarValue 'vmin'
const SEL_VMAX: u32 = 0x766d_6178; // kAudioVolumeControlPropertyMaximumScalarValue 'vmax'
const SEL_MUTE: u32 = 0x6d75_7465; // kAudioMuteControlPropertyValue 'mute'
const SEL_LCDV: u32 = 0x6c63_6476; // kAudioLevelControlPropertyDecibelValue 'lcdv'
const SEL_LCDR: u32 = 0x6c63_6472; // kAudioLevelControlPropertyDecibelRange 'lcdr'
const SEL_VDSP: u32 = 0x7664_7370; // 自定义 DSP 参数 'vdsp'：4×f32（gain/low/mid/high dB）
const SEL_VRUT: u32 = 0x7672_7574; // 自定义路由矩阵 'vrut'：4×f32（r00,r01,r10,r11）
const SEL_CUST: u32 = 0x6375_7374; // kAudioObjectPropertyCustomPropertyInfoList 'cust'

const SCOPE_GLOBAL: u32 = 0x676c_6f62; // 'glob'
const SCOPE_INPUT: u32 = 0x696e_7074; // 'inpt'
const SCOPE_OUTPUT: u32 = 0x6f75_7470; // 'outp'

const CLASS_OBJECT: u32 = 0x616f_626a; // kAudioObjectClassID 'aobj'
const CLASS_PLUGIN: u32 = 0x6170_6c67; // kAudioPlugInClassID 'aplg'
const CLASS_BOX: u32 = 0x6162_6f78;    // kAudioBoxClassID 'abox'
const CLASS_DEVICE: u32 = 0x6164_6576; // kAudioDeviceClassID 'adev'
const CLASS_STREAM: u32 = 0x6173_7472; // kAudioStreamClassID 'astr'
const CLASS_VOLUME: u32 = 0x766c_6d65; // kAudioVolumeControlClassID 'vlme'
const CLASS_MUTE: u32 = 0x6d75_7465;   // kAudioMuteControlClassID 'mute'

const CLOCK_ALGO_RAW: u32 = 0x7261_7777; // kAudioDeviceClockAlgorithmRaw 'raww'
const LAYOUT_71: u32 = 0x0080_0008; // kAudioChannelLayoutTag_MPEG_7_1_C（L R C LFE Ls Rs Rls Rrs）

const TERM_SPEAKER: u32 = 0x7370_6b72; // 'spkr'
const TERM_MIC: u32 = 0x6d69_6372; // 'micr'
const TRANSPORT_VIRTUAL: u32 = 0x7669_7274; // 'virt'

// CoreFoundation FFI
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: *const c_void;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        cstr: *const std::ffi::c_char,
        encoding: u32,
    ) -> *mut c_void;
    fn CFStringGetCString(
        s: *const c_void,
        buf: *mut std::ffi::c_char,
        len: isize,
        encoding: u32,
    ) -> bool;
    fn CFURLCreateWithBytes(
        alloc: *const c_void,
        url_bytes: *const u8,
        length: isize,
        encoding: u32,
        base_url: *const c_void,
    ) -> *mut c_void;
    // 仅测试用于校验 'icon' 返回对象类型（CFURLGetTypeID）
    #[cfg(test)]
    fn CFGetTypeID(cf: *const c_void) -> usize;
    #[cfg(test)]
    fn CFURLGetTypeID() -> usize;
}
const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8

fn cf_string(s: &str) -> *mut c_void {
    let c = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), UTF8) }
}

// kAudioDevicePropertyIcon 契约是 CFURLRef（不是 CFString）：返回驱动 bundle 内
// 图标占位路径的 file:// URL。路径允许不存在（宿主加载失败显示缺省图标，无害）；
// 返回 +1 引用，宿主负责释放。
// 测试钩子（仅测试构建）：CFURL 构造计数，回归锁定"'icon' 尺寸检查先于构造"。
#[cfg(test)]
static ICON_URL_BUILT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static ICON_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cf_icon_url() -> *mut c_void {
    #[cfg(test)]
    ICON_URL_BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let url = "file:///Library/Audio/Plug-Ins/HAL/vdev-audio.driver/Contents/Resources/vdev-audio.icns";
    unsafe {
        CFURLCreateWithBytes(
            kCFAllocatorDefault,
            url.as_ptr(),
            url.len() as isize,
            UTF8,
            std::ptr::null(),
        )
    }
}

fn asbd(rate: f64) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        m_sample_rate: rate,
        m_format_id: 0x6c70_636d, // 'lpcm'
        m_format_flags: 0x09,    // IsFloat | IsPacked
        m_bytes_per_packet: 32,
        m_frames_per_packet: 1,
        m_bytes_per_frame: 32,
        m_channels_per_frame: 8,
        m_bits_per_channel: 32,
        m_reserved: 0,
    }
}


const NO_ERR: OSStatus = 0;
// 错误码用 AudioHardwareBase.h 四字码的十六进制字面量（原十进制值系编造，已修正）；
// 回归：tests::test_error_code_four_char_codes 按四字码 ASCII 逐字静态断言。
const BAD_OBJ: OSStatus = 0x216F_626A_u32 as i32; // '!obj' kAudioHardwareBadObjectError（560947818）
const BAD_PROP: OSStatus = 0x7768_6F3F_u32 as i32; // 'who?' kAudioHardwareUnknownPropertyError（2003332927）
const BAD_SIZE: OSStatus = 0x2173_697A_u32 as i32; // '!siz' kAudioHardwareBadPropertySizeError（561211770）
const BAD_SEL: OSStatus = 0x756E_6F70_u32 as i32; // 'unop' kAudioHardwareUnsupportedOperationError（1970171760）

fn write_out<T: Copy>(out: *mut c_void, data_size: u32, out_size: *mut u32, val: T) -> OSStatus {
    let need = std::mem::size_of::<T>() as u32;
    if !out_size.is_null() { unsafe { *out_size = need; } }
    if data_size < need { return BAD_SIZE; }
    if !out.is_null() {
        // SAFETY：out 非空时宿主保证可写 data_size 字节，need ≤ data_size
        unsafe {
            std::ptr::copy_nonoverlapping((&raw const val).cast::<u8>(), out.cast::<u8>(), need as usize);
        }
    }
    NO_ERR
}

fn write_bytes_partial(out: *mut c_void, data_size: u32, out_size: *mut u32, bytes: &[u8]) -> OSStatus {
    // 数组属性：inDataSize 不足时截断返回（HAL 允许部分返回）
    let write = bytes.len().min(data_size as usize);
    if !out_size.is_null() { unsafe { *out_size = write as u32; } }
    if !out.is_null() && write > 0 {
        // SAFETY：out 非空时宿主保证可写 data_size 字节，write ≤ data_size
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), write);
        }
    }
    NO_ERR
}

fn write_cfstring(out: *mut c_void, data_size: u32, out_size: *mut u32, s: &str) -> OSStatus {
    // 返回 +1 CFStringRef，宿主负责释放
    let cf = cf_string(s);
    if !out_size.is_null() { unsafe { *out_size = std::mem::size_of::<*mut c_void>() as u32; } }
    if data_size < std::mem::size_of::<*mut c_void>() as u32 { return BAD_SIZE; }
    if !out.is_null() {
        // SAFETY：out 非空时宿主保证可容纳一个指针宽度
        unsafe {
            std::ptr::copy_nonoverlapping((&raw const cf).cast::<u8>(), out.cast::<u8>(), std::mem::size_of::<*mut c_void>());
        }
    }
    NO_ERR
}

// ---- 多设备辅助：对象类型判断 + 按设备取值 ----
// （is_dev/is_stream_out/is_stream/is_vol/is_mute 暂无调用点，作为与 is_stream_in
//   成套的对象类型判断集合刻意保留，不删除；is_dev 原被 has_property 的 'stm#'
//   scope 特例使用，M9 统一口径后特例移除）
#[allow(dead_code)]
fn is_dev(obj: AudioObjectID) -> bool { matches!(obj, DEV_A | DEV_B) }
#[allow(dead_code)]
fn is_stream_out(obj: AudioObjectID) -> bool { matches!(obj, A_OUT | B_OUT) }
fn is_stream_in(obj: AudioObjectID) -> bool { matches!(obj, A_IN | B_IN) }
#[allow(dead_code)]
fn is_stream(obj: AudioObjectID) -> bool { is_stream_out(obj) || is_stream_in(obj) }
#[allow(dead_code)]
fn is_vol(obj: AudioObjectID) -> bool { matches!(obj, A_VOL | B_VOL) }
#[allow(dead_code)]
fn is_mute(obj: AudioObjectID) -> bool { matches!(obj, A_MUTE | B_MUTE) }
fn dev_name(obj: AudioObjectID) -> &'static str {
    DEVS[dev_index(obj).unwrap_or(0)].name
}
fn dev_uid(obj: AudioObjectID) -> &'static str {
    DEVS[dev_index(obj).unwrap_or(0)].uid
}
fn dev_out(obj: AudioObjectID) -> AudioObjectID { if dev_index(obj) == Some(0) { A_OUT } else { B_OUT } }
fn dev_in(obj: AudioObjectID) -> AudioObjectID { if dev_index(obj) == Some(0) { A_IN } else { B_IN } }
fn dev_vol(obj: AudioObjectID) -> AudioObjectID { if dev_index(obj) == Some(0) { A_VOL } else { B_VOL } }
fn dev_mute(obj: AudioObjectID) -> AudioObjectID { if dev_index(obj) == Some(0) { A_MUTE } else { B_MUTE } }
fn dev_id(obj: AudioObjectID) -> AudioObjectID { if dev_index(obj) == Some(0) { DEV_A } else { DEV_B } }
// TranslateUIDToDevice：按 UID qualifier（CFString）映射到设备
fn translate_uid(qdata: *const c_void) -> AudioObjectID {
    if qdata.is_null() { return DEV_A; }
    // SAFETY：qdata 非空时按 HAL 契约指向一个 CFStringRef qualifier
    let cf = unsafe { *qdata.cast::<*mut c_void>() };
    let mut buf = [0 as std::ffi::c_char; 128];
    if !cf.is_null() && unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), 128, UTF8) } {
        let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
        for (i, meta) in DEVS.iter().enumerate() {
            if s.contains(meta.uid) { return if i == 0 { DEV_A } else { DEV_B }; }
        }
    }
    DEV_A
}

unsafe extern "C" fn plugin_has_property(
    _driver: AudioServerPlugInDriverRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: *const AudioObjectPropertyAddress,
) -> Boolean {
    if addr.is_null() { return 0; }
    let sel = unsafe { (*addr).m_selector };
    let ok = match obj {
        OBJ_PLUGIN => matches!(sel, SEL_PMFR | SEL_PNAM | SEL_PVER | SEL_PBOX | SEL_PDEV | SEL_UIDB | SEL_UIDD | SEL_RSRC | SEL_LNAM | SEL_LMOD | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND),
        OBJ_BOX => matches!(sel, SEL_BUID | SEL_BTRN | SEL_BHAU | SEL_BHVI | SEL_BHMI | SEL_BPRO | SEL_BXON | SEL_BXOF | SEL_BDV | SEL_BNAM | SEL_BMFR | SEL_BMOD | SEL_BSNO | SEL_BFMW | SEL_LNAM | SEL_LMOD | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_IDEN | SEL_SNUM | SEL_FWVN),
        DEV_A | DEV_B => matches!(sel, SEL_UID | SEL_MUID | SEL_TRAN | SEL_GROU | SEL_CLKD | SEL_LIVN | SEL_GOIN | SEL_GONE | SEL_DFLT | SEL_SFLT | SEL_LTNC | SEL_STM | SEL_CTRL | SEL_SAFT | SEL_NSRT | SEL_NSR | SEL_HIDN | SEL_FSIZ | SEL_FSZ | SEL_VFSZ | SEL_DCH2 | SEL_LNAM | SEL_LMOD | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_RING | SEL_CSTB | SEL_CLOK | SEL_ICON | SEL_SRND | SEL_VDSP | SEL_VRUT | SEL_CUST),
        A_OUT | A_IN | B_OUT | B_IN => matches!(sel, SEL_SACT | SEL_SDIR | SEL_TERM | SEL_SCHN | SEL_LTNC | SEL_SFMT | SEL_PFT | SEL_SFMA | SEL_PFTA | SEL_LNAM | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND),
        A_VOL | B_VOL => matches!(sel, SEL_STBL | SEL_VLSC | SEL_VMIN | SEL_VMAX | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_CSCP | SEL_CELM | SEL_LCDV | SEL_LCDR),
        A_MUTE | B_MUTE => matches!(sel, SEL_STBL | SEL_MUTE | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_CSCP | SEL_CELM),
        _ => false,
    };
    // 'stm#' 对全部 scope 可用（与 Get/Size 同口径）：glob=全部流，input/output=本向流
    u8::from(ok)
}

unsafe extern "C" fn plugin_is_property_settable(
    _driver: AudioServerPlugInDriverRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: *const AudioObjectPropertyAddress,
    out: *mut u8,
) -> OSStatus {
    if addr.is_null() || out.is_null() { return BAD_SEL; }
    let sel = unsafe { (*addr).m_selector };
    let settable = match obj {
        DEV_A | DEV_B => matches!(sel, SEL_NSRT | SEL_FSIZ | SEL_VDSP | SEL_VRUT),
        A_VOL | B_VOL => sel == SEL_VLSC,
        A_MUTE | B_MUTE => sel == SEL_MUTE,
        _ => false,
    };
    unsafe { *out = u8::from(settable); }
    NO_ERR
}

// 属性分发表按 CoreAudio selector 逐项列出：同值分支刻意不合并（可 grep、可对照 C 头）
#[allow(clippy::match_same_arms)]
unsafe extern "C" fn plugin_get_property_data_size(
    _driver: AudioServerPlugInDriverRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: *const AudioObjectPropertyAddress,
    _qsize: u32,
    _qdata: *const c_void,
    out_size: *mut u32,
) -> OSStatus {
    if addr.is_null() || out_size.is_null() { return BAD_SEL; }
    let sel = unsafe { (*addr).m_selector };
    let scope = unsafe { (*addr).m_scope }; // 'stm#'/'ownd' 尺寸随 scope 变化（与 Get 同口径）
    let size: u32 = match obj {
        OBJ_PLUGIN => match sel {
            SEL_CLAS | SEL_BCLS | SEL_OWNE => 4,
            SEL_OWND => 4,
            SEL_PMFR | SEL_PNAM | SEL_PVER | SEL_RSRC | SEL_LNAM | SEL_LMOD | SEL_LMAK => std::mem::size_of::<*mut c_void>() as u32,
            SEL_PBOX => std::mem::size_of::<AudioObjectID>() as u32,
            SEL_PDEV => 2 * std::mem::size_of::<AudioObjectID>() as u32,
            SEL_UIDB | SEL_UIDD => std::mem::size_of::<AudioObjectID>() as u32,
            _ => return BAD_PROP,
        },
        OBJ_BOX => match sel {
            SEL_CLAS | SEL_BCLS | SEL_OWNE => 4,
            SEL_OWND => 0,
            SEL_SNUM | SEL_FWVN => std::mem::size_of::<*mut c_void>() as u32,
            SEL_IDEN => 4,
            SEL_BUID | SEL_BNAM | SEL_BMFR | SEL_BMOD | SEL_BSNO | SEL_BFMW | SEL_LNAM | SEL_LMOD | SEL_LMAK => std::mem::size_of::<*mut c_void>() as u32,
            SEL_BTRN | SEL_BHAU | SEL_BHVI | SEL_BHMI | SEL_BPRO | SEL_BXON | SEL_BXOF => 4,
            SEL_BDV => 2 * std::mem::size_of::<AudioObjectID>() as u32,
            _ => return BAD_PROP,
        },
        DEV_A | DEV_B => match sel {
            SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_RING | SEL_CSTB | SEL_CLOK => 4,
            SEL_VDSP => std::mem::size_of::<*mut c_void>() as u32,
            SEL_VRUT => std::mem::size_of::<*mut c_void>() as u32,
            SEL_CUST => 2 * std::mem::size_of::<CustomPropertyInfo>() as u32,
            SEL_SRND => 12, // AudioChannelLayout（tag+bitmap+count，无描述）
            SEL_ICON => std::mem::size_of::<*mut c_void>() as u32,
            // 'ownd'（M10）：按 scope 返回真实数量，与 Get 一致（glob=4，output=3，input=1）
            SEL_OWND => match scope {
                SCOPE_OUTPUT => 3 * std::mem::size_of::<AudioObjectID>() as u32,
                SCOPE_INPUT => std::mem::size_of::<AudioObjectID>() as u32,
                _ => 4 * std::mem::size_of::<AudioObjectID>() as u32,
            },
            SEL_UID | SEL_MUID | SEL_LNAM | SEL_LMOD | SEL_LMAK => std::mem::size_of::<*mut c_void>() as u32,
            SEL_TRAN | SEL_CLKD | SEL_DFLT | SEL_SFLT | SEL_LTNC | SEL_SAFT | SEL_HIDN | SEL_FSIZ | SEL_VFSZ | SEL_GOIN | SEL_GONE | SEL_LIVN => 4,
            SEL_GROU => std::mem::size_of::<AudioObjectID>() as u32,
            // 'stm#'（M9）：glob=全部流（输出+输入），input/output=单流（HAL 惯例 glob=全部）
            SEL_STM => match scope {
                SCOPE_GLOBAL => 2 * std::mem::size_of::<AudioObjectID>() as u32,
                _ => std::mem::size_of::<AudioObjectID>() as u32,
            },
            SEL_CTRL => 2 * std::mem::size_of::<AudioObjectID>() as u32,
            SEL_NSRT => 8,
            SEL_NSR => 2 * std::mem::size_of::<AudioValueRange>() as u32,
            SEL_FSZ => std::mem::size_of::<AudioValueRange>() as u32,
            SEL_DCH2 => 2 * 4,
            _ => return BAD_PROP,
        },
        A_OUT | A_IN | B_OUT | B_IN => match sel {
            SEL_CLAS | SEL_BCLS | SEL_OWNE => 4,
            SEL_OWND => 0,
            SEL_LNAM | SEL_LMAK => std::mem::size_of::<*mut c_void>() as u32,
            SEL_SACT | SEL_SDIR | SEL_TERM | SEL_SCHN | SEL_LTNC => 4,
            SEL_SFMT | SEL_PFT => std::mem::size_of::<AudioStreamBasicDescription>() as u32,
            SEL_SFMA | SEL_PFTA => 2 * std::mem::size_of::<AudioStreamBasicDescription>() as u32,
            _ => return BAD_PROP,
        },
        A_VOL | B_VOL => match sel {
            SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_CSCP | SEL_CELM | SEL_LCDV => 4,
            SEL_LCDR => 2 * std::mem::size_of::<AudioValueRange>() as u32,
            SEL_OWND => 0,
            SEL_STBL => 1,
            SEL_VLSC | SEL_VMIN | SEL_VMAX => 4,
            _ => return BAD_PROP,
        },
        A_MUTE | B_MUTE => match sel {
            SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_CSCP | SEL_CELM => 4,
            SEL_OWND => 0,
            SEL_STBL => 1,
            SEL_MUTE => 4,
            _ => return BAD_PROP,
        },
        _ => return BAD_OBJ,
    };
    unsafe { *out_size = size; }
    NO_ERR
}

// 属性分发表按 CoreAudio selector 逐项列出：同值分支刻意不合并（可 grep、可对照 C 头）；
// 行数来自完整 selector 覆盖，拆分反而破坏对照可读性。
#[allow(clippy::match_same_arms, clippy::too_many_lines)]
unsafe extern "C" fn plugin_get_property_data(
    _driver: AudioServerPlugInDriverRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: *const AudioObjectPropertyAddress,
    _qsize: u32,
    qdata: *const c_void,
    data_size: u32,
    out_size: *mut u32,
    out: *mut c_void,
) -> OSStatus {
    if addr.is_null() { return BAD_SEL; }
    let sel = unsafe { (*addr).m_selector };
    let scope = unsafe { (*addr).m_scope };
    match obj {
        OBJ_PLUGIN => match sel {
            SEL_CLAS => write_out(out, data_size, out_size, CLASS_PLUGIN),
            SEL_BCLS => write_out(out, data_size, out_size, CLASS_OBJECT),
            SEL_OWNE => write_out(out, data_size, out_size, 0u32),
            SEL_OWND => write_out(out, data_size, out_size, OBJ_BOX),
            SEL_PMFR | SEL_LMAK => write_cfstring(out, data_size, out_size, "vdev"),
            SEL_PNAM | SEL_LNAM => write_cfstring(out, data_size, out_size, "vdev-audio"),
            SEL_LMOD => write_cfstring(out, data_size, out_size, "vdev-audio"),
            SEL_PVER => write_cfstring(out, data_size, out_size, "1.0.0"),
            SEL_RSRC => write_cfstring(out, data_size, out_size, "com.vdev.audio.driver"),
            SEL_PBOX => write_out(out, data_size, out_size, OBJ_BOX),
            SEL_PDEV => {
                let devs = [DEV_A, DEV_B];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(devs.as_ptr().cast::<u8>(), 2 * std::mem::size_of::<AudioObjectID>())
                })
            }
            SEL_UIDB => write_out(out, data_size, out_size, OBJ_BOX),
            SEL_UIDD => write_out(out, data_size, out_size, translate_uid(qdata)),
            _ => BAD_PROP,
        },
        OBJ_BOX => match sel {
            SEL_CLAS => write_out(out, data_size, out_size, CLASS_BOX),
            SEL_IDEN => write_out(out, data_size, out_size, 0u32),
            SEL_SNUM => write_cfstring(out, data_size, out_size, "1"),
            SEL_FWVN => write_cfstring(out, data_size, out_size, "1.0.0"),
            SEL_BCLS => write_out(out, data_size, out_size, CLASS_OBJECT),
            SEL_OWNE => write_out(out, data_size, out_size, OBJ_PLUGIN),
            SEL_OWND => { unsafe { if !out_size.is_null() { *out_size = 0; } } NO_ERR }
            SEL_BUID => write_cfstring(out, data_size, out_size, "vdev-audio-box"),
            SEL_BNAM | SEL_LNAM => write_cfstring(out, data_size, out_size, "vdev-audio"),
            SEL_BMFR | SEL_LMAK => write_cfstring(out, data_size, out_size, "vdev"),
            SEL_LMOD => write_cfstring(out, data_size, out_size, "vdev-audio 2ch"),
            SEL_BMOD => write_cfstring(out, data_size, out_size, "vdev-audio 2ch"),
            SEL_BSNO => write_cfstring(out, data_size, out_size, "1"),
            SEL_BFMW => write_cfstring(out, data_size, out_size, "1.0.0"),
            SEL_BTRN => write_out(out, data_size, out_size, TRANSPORT_VIRTUAL),
            SEL_BHAU => write_out(out, data_size, out_size, 1u32),
            SEL_BHVI | SEL_BHMI | SEL_BPRO | SEL_BXOF => write_out(out, data_size, out_size, 0u32),
            SEL_BXON => write_out(out, data_size, out_size, 1u32),
            SEL_BDV => {
                let devs = [DEV_A, DEV_B];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(devs.as_ptr().cast::<u8>(), 2 * std::mem::size_of::<AudioObjectID>())
                })
            }
            _ => BAD_PROP,
        },
        DEV_A | DEV_B => match sel {
            SEL_CLAS => write_out(out, data_size, out_size, CLASS_DEVICE),
            SEL_BCLS => write_out(out, data_size, out_size, CLASS_OBJECT),
            SEL_OWNE => write_out(out, data_size, out_size, OBJ_PLUGIN),
            SEL_CUST => {
                let info = [
                    CustomPropertyInfo { m_selector: SEL_VDSP, m_data_type: 0x6366_7374 /* 'cfst' */, m_qualifier_data_type: 0 },
                    CustomPropertyInfo { m_selector: SEL_VRUT, m_data_type: 0x6366_7374 /* 'cfst' */, m_qualifier_data_type: 0 },
                ];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(info.as_ptr().cast::<u8>(), 2 * std::mem::size_of::<CustomPropertyInfo>())
                })
            }
            SEL_VRUT => {
                let rows = route_rows();
                let s = format!(
                    "{:.1},{:.1},{:.1},{:.1}",
                    route_row_lane(rows[0], 0),
                    route_row_lane(rows[0], 1),
                    route_row_lane(rows[1], 0),
                    route_row_lane(rows[1], 1)
                );
                write_cfstring(out, data_size, out_size, &s)
            }
            SEL_VDSP => {
                let p = dsp(dev_index(obj).unwrap_or(0))
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .params();
                let s = format!("{:.1},{:.1},{:.1},{:.1}", p[0], p[1], p[2], p[3]);
                write_cfstring(out, data_size, out_size, &s)
            }
            SEL_RING => write_out(out, data_size, out_size, 16384u32),
            SEL_CSTB => write_out(out, data_size, out_size, 1u32),
            SEL_CLOK => write_out(out, data_size, out_size, CLOCK_ALGO_RAW),
            SEL_SRND => {
                // AudioChannelLayout：stereo tag，无 channel descriptions
                let layout: [u32; 3] = [LAYOUT_71, 0, 0];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(layout.as_ptr().cast::<u8>(), 12)
                })
            }
            SEL_ICON => {
                // 与 write_cfstring 同序：先回填 out_size 并校验 data_size，不足
                // 时直接 BAD_SIZE——cf_icon_url() 会创建 +1 CFURL，白建即泄漏，
                // 必须等尺寸检查通过再构造。
                if !out_size.is_null() {
                    unsafe { *out_size = std::mem::size_of::<*mut c_void>() as u32 };
                }
                if data_size < std::mem::size_of::<*mut c_void>() as u32 {
                    return BAD_SIZE;
                }
                write_out(out, data_size, out_size, cf_icon_url())
            }
            SEL_OWND => {
                let mut owned: Vec<u32> = Vec::new();
                if scope == SCOPE_GLOBAL || scope == SCOPE_OUTPUT {
                    owned.push(dev_out(obj));
                    owned.push(dev_vol(obj));
                    owned.push(dev_mute(obj));
                }
                if scope == SCOPE_GLOBAL || scope == SCOPE_INPUT {
                    owned.push(dev_in(obj));
                }
                let bytes = unsafe { std::slice::from_raw_parts(owned.as_ptr().cast::<u8>(), owned.len() * 4) };
                write_bytes_partial(out, data_size, out_size, bytes)
            }
            SEL_UID => write_cfstring(out, data_size, out_size, dev_uid(obj)),
            SEL_MUID => write_cfstring(out, data_size, out_size, dev_uid(obj)),
            SEL_LNAM => write_cfstring(out, data_size, out_size, dev_name(obj)),
            SEL_LMOD => write_cfstring(out, data_size, out_size, "vdev-audio 8ch"),
            SEL_LMAK => write_cfstring(out, data_size, out_size, "vdev"),
            SEL_TRAN => write_out(out, data_size, out_size, TRANSPORT_VIRTUAL),
            SEL_GROU => write_out(out, data_size, out_size, dev_id(obj)),
            SEL_CLKD => write_out(out, data_size, out_size, 0u32),
            SEL_LIVN => write_out(out, data_size, out_size, 1u32),
            SEL_GOIN | SEL_GONE => write_out(out, data_size, out_size, device_running(obj)),
            SEL_DFLT | SEL_SFLT => write_out(out, data_size, out_size, 1u32),
            SEL_LTNC | SEL_SAFT => write_out(out, data_size, out_size, 0u32),
            SEL_HIDN => write_out(out, data_size, out_size, 0u32),
            SEL_FSIZ => write_out(out, data_size, out_size, 512u32),
            SEL_VFSZ => write_out(out, data_size, out_size, 0u32),
            SEL_FSZ => write_out(out, data_size, out_size, AudioValueRange { m_minimum: 512.0, m_maximum: 512.0 }),
            SEL_NSRT => write_out(out, data_size, out_size, SAMPLE_RATE.load(Ordering::SeqCst) as f64),
            SEL_NSR => {
                let rates = [
                    AudioValueRange { m_minimum: 44100.0, m_maximum: 44100.0 },
                    AudioValueRange { m_minimum: 48000.0, m_maximum: 48000.0 },
                ];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(rates.as_ptr().cast::<u8>(), std::mem::size_of::<[AudioValueRange; 2]>())
                })
            }
            // 'stm#'（M9）：glob=全部流（先输出后输入），input=输入流，output=输出流
            SEL_STM => match scope {
                SCOPE_INPUT => write_out(out, data_size, out_size, dev_in(obj)),
                SCOPE_GLOBAL => {
                    let streams = [dev_out(obj), dev_in(obj)];
                    write_bytes_partial(out, data_size, out_size, unsafe {
                        std::slice::from_raw_parts(
                            streams.as_ptr().cast::<u8>(),
                            std::mem::size_of::<[AudioObjectID; 2]>(),
                        )
                    })
                }
                _ => write_out(out, data_size, out_size, dev_out(obj)),
            }
            SEL_CTRL => {
                let ctrls = [dev_vol(obj), dev_mute(obj)];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(ctrls.as_ptr().cast::<u8>(), std::mem::size_of::<[AudioObjectID; 2]>())
                })
            }
            SEL_DCH2 => {
                let chs = [1u32, 2u32];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(chs.as_ptr().cast::<u8>(), 8)
                })
            }
            _ => BAD_PROP,
        },
        A_OUT | A_IN | B_OUT | B_IN => {
            let input = is_stream_in(obj);
            match sel {
                SEL_CLAS => write_out(out, data_size, out_size, CLASS_STREAM),
                SEL_BCLS => write_out(out, data_size, out_size, CLASS_OBJECT),
                SEL_OWNE => write_out(out, data_size, out_size, dev_id(obj)),
                SEL_OWND => { unsafe { if !out_size.is_null() { *out_size = 0; } } NO_ERR }
                SEL_LNAM => write_cfstring(out, data_size, out_size, if input { "vdev-audio Input" } else { "vdev-audio Output" }),
                SEL_LMAK => write_cfstring(out, data_size, out_size, "vdev"),
                SEL_SACT => write_out(out, data_size, out_size, 1u32),
                SEL_SDIR => write_out(out, data_size, out_size, u32::from(input)),
                SEL_TERM => write_out(out, data_size, out_size, if input { TERM_MIC } else { TERM_SPEAKER }),
                SEL_SCHN => write_out(out, data_size, out_size, 1u32),
                SEL_LTNC => write_out(out, data_size, out_size, 0u32),
                SEL_SFMT | SEL_PFT => {
                    let fmt = asbd(SAMPLE_RATE.load(Ordering::SeqCst) as f64);
                    write_out(out, data_size, out_size, fmt)
                }
                SEL_SFMA | SEL_PFTA => {
                    let fmts = [asbd(44100.0), asbd(48000.0)];
                    write_bytes_partial(out, data_size, out_size, unsafe {
                        std::slice::from_raw_parts(fmts.as_ptr().cast::<u8>(), std::mem::size_of::<[AudioStreamBasicDescription; 2]>())
                    })
                }
                _ => BAD_PROP,
            }
        }
        A_VOL | B_VOL => match sel {
            SEL_CLAS => write_out(out, data_size, out_size, CLASS_VOLUME),
            SEL_BCLS => write_out(out, data_size, out_size, CLASS_OBJECT),
            SEL_OWNE => write_out(out, data_size, out_size, dev_id(obj)),
            SEL_OWND => { unsafe { if !out_size.is_null() { *out_size = 0; } } NO_ERR },
            SEL_LCDV => write_out(out, data_size, out_size, 0.0f32),
            SEL_LCDR => {
                let r = [AudioValueRange { m_minimum: -96.0, m_maximum: 0.0 }];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(r.as_ptr().cast::<u8>(), std::mem::size_of::<AudioValueRange>())
                })
            }
            SEL_CSCP => write_out(out, data_size, out_size, SCOPE_OUTPUT),
            SEL_CELM => write_out(out, data_size, out_size, 1u32),
            SEL_STBL => write_out(out, data_size, out_size, 1u8),
            SEL_VLSC => write_out(out, data_size, out_size, 1.0f32),
            SEL_VMIN => write_out(out, data_size, out_size, 0.0f32),
            SEL_VMAX => write_out(out, data_size, out_size, 1.0f32),
            _ => BAD_PROP,
        },
        A_MUTE | B_MUTE => match sel {
            SEL_CLAS => write_out(out, data_size, out_size, CLASS_MUTE),
            SEL_CSCP => write_out(out, data_size, out_size, SCOPE_OUTPUT),
            SEL_CELM => write_out(out, data_size, out_size, 1u32),
            SEL_BCLS => write_out(out, data_size, out_size, CLASS_OBJECT),
            SEL_OWNE => write_out(out, data_size, out_size, dev_id(obj)),
            SEL_OWND => { unsafe { if !out_size.is_null() { *out_size = 0; } } NO_ERR }
            SEL_STBL => write_out(out, data_size, out_size, 1u8),
            SEL_MUTE => write_out(out, data_size, out_size, 0.0f32),
            _ => BAD_PROP,
        },
        _ => BAD_OBJ,
    }
}

unsafe extern "C" fn plugin_set_property_data(
    _driver: AudioServerPlugInDriverRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: *const AudioObjectPropertyAddress,
    _qsize: u32,
    _qdata: *const c_void,
    _data_size: u32,
    data: *const c_void,
) -> OSStatus {
    if addr.is_null() || data.is_null() { return BAD_SEL; }
    let sel = unsafe { (*addr).m_selector };
    match obj {
        DEV_A | DEV_B if sel == SEL_NSRT => {
            let rate = unsafe { *data.cast::<f64>() };
            // 支持采样率白名单：宿主传的就是字面量，非浮点运算结果
            #[allow(clippy::float_cmp)]
            if rate != 44100.0 && rate != 48000.0 {
                return BAD_PROP;
            }
            SAMPLE_RATE.store(rate as u64, Ordering::SeqCst);
            NO_ERR
        }
        DEV_A | DEV_B if sel == SEL_FSIZ => NO_ERR, // 接受任意 buffer size
        DEV_A | DEV_B if sel == SEL_VDSP => {
            // data 是 CFStringRef："gain,low,mid,high"
            let cf = unsafe { *data.cast::<*mut c_void>() };
            let mut buf = [0 as std::ffi::c_char; 128];
            if cf.is_null() || !unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), 128, UTF8) } {
                return BAD_PROP;
            }
            let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
            let parts: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if parts.len() != 4 {
                return BAD_PROP;
            }
            let rate = SAMPLE_RATE.load(Ordering::SeqCst) as f32;
            dsp(dev_index(obj).unwrap_or(0))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_params(parts[0], parts[1], parts[2], parts[3], rate);
            NO_ERR
        }
        DEV_A | DEV_B if sel == SEL_VRUT => {
            let cf = unsafe { *data.cast::<*mut c_void>() };
            let mut buf = [0 as std::ffi::c_char; 128];
            if cf.is_null() || !unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), 128, UTF8) } {
                return BAD_PROP;
            }
            let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
            let parts: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if parts.len() != 4 {
                return BAD_PROP;
            }
            // 每行一次原子 store（M4：RT 读侧无锁整行快照，无撕裂）
            route_set_row(0, [parts[0], parts[1]]);
            route_set_row(1, [parts[2], parts[3]]);
            NO_ERR
        }
        A_VOL | B_VOL if sel == SEL_VLSC => NO_ERR,
        A_MUTE | B_MUTE if sel == SEL_MUTE => NO_ERR,
        _ => BAD_PROP,
    }
}
