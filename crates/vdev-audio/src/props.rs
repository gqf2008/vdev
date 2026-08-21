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

const SEL_LNAM: u32 = 0x6c6e616d; // kAudioObjectPropertyName 'lnam' 
const SEL_LMOD: u32 = 0x6c6d6f64; // kAudioObjectPropertyModelName 'lmod'
const SEL_LMAK: u32 = 0x6c6d616b; // kAudioObjectPropertyManufacturer 'lmak'
const SEL_CLAS: u32 = 0x636c6173; // kAudioObjectPropertyClass 'clas'
const SEL_BCLS: u32 = 0x62636c73; // kAudioObjectPropertyBaseClass 'bcls'
const SEL_OWNE: u32 = 0x73746476; // kAudioObjectPropertyOwner 'stdv' (macOS 26)
const SEL_OWND: u32 = 0x6f776e64; // kAudioObjectPropertyOwnedObjects 'ownd'
const SEL_RING: u32 = 0x72696e67; // kAudioDevicePropertyZeroTimeStampPeriod 'ring'
const SEL_CSCP: u32 = 0x63736370; // kAudioControlPropertyScope 'cscp'
const SEL_CELM: u32 = 0x63656c6d; // kAudioControlPropertyElement 'celm'
const SEL_CSTB: u32 = 0x63737462; // kAudioDevicePropertyClockIsStable 'cstb'

// ---- plug-in ----
const SEL_PMFR: u32 = 0x706d6672; // kAudioPlugInPropertyManufacturer 'pmfr'
const SEL_PNAM: u32 = 0x706e616d; // kAudioPlugInPropertyName 'pnam'
const SEL_PVER: u32 = 0x70766572; // kAudioPlugInPropertyVersion 'pver'
const SEL_RSRC: u32 = 0x72737263; // kAudioPlugInPropertyResourceBundle 'rsrc'
const SEL_PBOX: u32 = 0x626f7823; // kAudioPlugInPropertyBoxList 'box#'
const SEL_PDEV: u32 = 0x64657623; // kAudioPlugInPropertyDeviceList 'dev#'
const SEL_UIDB: u32 = 0x75696462; // TranslateUIDToBox 'uidb'
const SEL_UIDD: u32 = 0x75696464; // TranslateUIDToDevice 'uidd'
// ---- box ----
const SEL_BUID: u32 = 0x62756964; // kAudioBoxPropertyBoxUID 'buid'
const SEL_BTRN: u32 = 0x7472616e; // kAudioBoxPropertyTransportType 'tran'
const SEL_BHAU: u32 = 0x62686175; // kAudioBoxPropertyHasAudio 'bhau'
const SEL_BHVI: u32 = 0x62687669; // kAudioBoxPropertyHasVideo 'bhvi'
const SEL_BHMI: u32 = 0x62686d69; // kAudioBoxPropertyHasMIDI 'bhmi'
const SEL_BPRO: u32 = 0x6270726f; // kAudioBoxPropertyIsProtected 'bpro'
const SEL_BXON: u32 = 0x62786f6e; // kAudioBoxPropertyAcquired 'bxon'
const SEL_BXOF: u32 = 0x62786f66; // kAudioBoxPropertyAcquisitionFailed 'bxof'
const SEL_BDV: u32 = 0x62647623; // kAudioBoxPropertyDeviceList 'bdv#'
const SEL_BNAM: u32 = 0x626e616d; // kAudioBoxPropertyName 'bnam'
const SEL_BMFR: u32 = 0x626d6672; // kAudioBoxPropertyManufacturer 'bmfr'
const SEL_BMOD: u32 = 0x626d6f64; // kAudioBoxPropertyModel 'bmod'
const SEL_BSNO: u32 = 0x62736e6f; // kAudioBoxPropertySerialNumber 'bsno'
const SEL_BFMW: u32 = 0x62666d77; // kAudioBoxPropertyFirmwareVersion 'bfmw'
const SEL_IDEN: u32 = 0x6964656e; // kAudioObjectPropertyIdentify 'iden'
const SEL_SNUM: u32 = 0x736e756d; // kAudioObjectPropertySerialNumber 'snum'
const SEL_FWVN: u32 = 0x6677766e; // kAudioObjectPropertyFirmwareVersion 'fwvn'
// ---- device ----
const SEL_UID: u32 = 0x75696420; // kAudioDevicePropertyDeviceUID 'uid '
const SEL_MUID: u32 = 0x6d756964; // kAudioDevicePropertyModelUID 'muid'
const SEL_ICON: u32 = 0x69636f6e; // kAudioDevicePropertyIcon 'icon'
const SEL_SRND: u32 = 0x73726e64; // kAudioDevicePropertyPreferredChannelLayout 'srnd'
const SEL_TRAN: u32 = 0x7472616e; // kAudioDevicePropertyTransportType 'tran'
const SEL_GROU: u32 = 0x67726f75; // kAudioDevicePropertyRelatedDevices 'grou'
const SEL_CLKD: u32 = 0x636c6b64; // kAudioDevicePropertyClockDomain 'clkd'
const SEL_CLOK: u32 = 0x636c6f6b; // kAudioDevicePropertyClockAlgorithm 'clok' (macOS 26)
const SEL_LIVN: u32 = 0x6c69766e; // kAudioDevicePropertyDeviceIsAlive 'livn'
const SEL_GOIN: u32 = 0x676f696e; // kAudioDevicePropertyDeviceIsRunning 'goin'
const SEL_GONE: u32 = 0x676f6e65; // kAudioDevicePropertyDeviceIsRunningSomewhere 'gone'
const SEL_DFLT: u32 = 0x64666c74; // kAudioDevicePropertyDeviceCanBeDefaultDevice 'dflt'
const SEL_SFLT: u32 = 0x73666c74; // kAudioDevicePropertyDeviceCanBeDefaultSystemDevice 'sflt'
const SEL_LTNC: u32 = 0x6c746e63; // kAudioDevicePropertyLatency 'ltnc'
const SEL_STM: u32 = 0x73746d23; // kAudioDevicePropertyStreams 'stm#'
const SEL_CTRL: u32 = 0x6374726c; // kAudioDevicePropertyControlList 'ctrl'
const SEL_SAFT: u32 = 0x73616674; // kAudioDevicePropertySafetyOffset 'saft'
const SEL_NSRT: u32 = 0x6e737274; // kAudioDevicePropertyNominalSampleRate 'nsrt'
const SEL_NSR: u32 = 0x6e737223; // kAudioDevicePropertyAvailableNominalSampleRates 'nsr#'
const SEL_HIDN: u32 = 0x6869646e; // kAudioDevicePropertyIsHidden 'hidn'
const SEL_FSIZ: u32 = 0x6673697a; // kAudioDevicePropertyBufferFrameSize 'fsiz'
const SEL_FSZ: u32 = 0x66737a23; // kAudioDevicePropertyBufferFrameSizeRange 'fsz#'
const SEL_VFSZ: u32 = 0x7666737a; // kAudioDevicePropertyUsesVariableBufferFrameSizes 'vfsz'
const SEL_DCH2: u32 = 0x64636832; // kAudioDevicePropertyPreferredChannelsForStereo 'dch2'
// ---- stream ----
const SEL_SACT: u32 = 0x73616374; // kAudioStreamPropertyIsActive 'sact'
const SEL_SDIR: u32 = 0x73646972; // kAudioStreamPropertyDirection 'sdir'
const SEL_TERM: u32 = 0x7465726d; // kAudioStreamPropertyTerminalType 'term'
const SEL_SCHN: u32 = 0x7363686e; // kAudioStreamPropertyStartingChannel 'schn'
const SEL_SFMT: u32 = 0x73666d74; // kAudioStreamPropertyVirtualFormat 'sfmt'
const SEL_PFT: u32 = 0x70667420; // kAudioStreamPropertyPhysicalFormat 'pft '
const SEL_SFMA: u32 = 0x73666d61; // kAudioStreamPropertyAvailableVirtualFormats 'sfma'
const SEL_PFTA: u32 = 0x70667461; // kAudioStreamPropertyAvailablePhysicalFormats 'pfta'
// ---- control ----
const SEL_STBL: u32 = 0x7374626c; // kAudioControlPropertyIsSettable 'stbl'
const SEL_VLSC: u32 = 0x766c7363; // kAudioVolumeControlPropertyScalarValue 'vlsc'
const SEL_VMIN: u32 = 0x766d696e; // kAudioVolumeControlPropertyMinimumScalarValue 'vmin'
const SEL_VMAX: u32 = 0x766d6178; // kAudioVolumeControlPropertyMaximumScalarValue 'vmax'
const SEL_MUTE: u32 = 0x6d757465; // kAudioMuteControlPropertyValue 'mute'
const SEL_LCDV: u32 = 0x6c636476; // kAudioLevelControlPropertyDecibelValue 'lcdv'
const SEL_LCDR: u32 = 0x6c636472; // kAudioLevelControlPropertyDecibelRange 'lcdr'
const SEL_VDSP: u32 = 0x76647370; // 自定义 DSP 参数 'vdsp'：4×f32（gain/low/mid/high dB）
const SEL_VRUT: u32 = 0x76727574; // 自定义路由矩阵 'vrut'：4×f32（r00,r01,r10,r11）
const SEL_CUST: u32 = 0x63757374; // kAudioObjectPropertyCustomPropertyInfoList 'cust'

const SCOPE_GLOBAL: u32 = 0x676c6f62; // 'glob'
const SCOPE_INPUT: u32 = 0x696e7074; // 'inpt'
const SCOPE_OUTPUT: u32 = 0x6f757470; // 'outp'

const CLASS_OBJECT: u32 = 0x616f626a; // kAudioObjectClassID 'aobj'
const CLASS_PLUGIN: u32 = 0x61706c67; // kAudioPlugInClassID 'aplg'
const CLASS_BOX: u32 = 0x61626f78;    // kAudioBoxClassID 'abox'
const CLASS_DEVICE: u32 = 0x61646576; // kAudioDeviceClassID 'adev'
const CLASS_STREAM: u32 = 0x61737472; // kAudioStreamClassID 'astr'
const CLASS_VOLUME: u32 = 0x766c6d65; // kAudioVolumeControlClassID 'vlme'
const CLASS_MUTE: u32 = 0x6d757465;   // kAudioMuteControlClassID 'mute'

const CLOCK_ALGO_RAW: u32 = 0x72617777; // kAudioDeviceClockAlgorithmRaw 'raww'
const LAYOUT_71: u32 = 0x00800008; // kAudioChannelLayoutTag_MPEG_7_1_C（L R C LFE Ls Rs Rls Rrs）

const TERM_SPEAKER: u32 = 0x73706b72; // 'spkr'
const TERM_MIC: u32 = 0x6d696372; // 'micr'
const TRANSPORT_VIRTUAL: u32 = 0x76697274; // 'virt'

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
}
const UTF8: u32 = 0x08000100; // kCFStringEncodingUTF8

fn cf_string(s: &str) -> *mut c_void {
    let c = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), UTF8) }
}

fn asbd(rate: f64) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        m_sample_rate: rate,
        m_format_id: 0x6c70636d, // 'lpcm'
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
const BAD_OBJ: OSStatus = -560557684; // kAudioHardwareBadObjectError
const BAD_PROP: OSStatus = -560557686; // kAudioHardwareUnknownPropertyError
const BAD_SIZE: OSStatus = -560557690; // kAudioHardwareBadPropertySizeError
const BAD_SEL: OSStatus = -560557681; // kAudioHardwareUnsupportedOperationError

fn write_out<T: Copy>(out: *mut c_void, data_size: u32, out_size: *mut u32, val: T) -> OSStatus {
    unsafe {
        let need = std::mem::size_of::<T>() as u32;
        if !out_size.is_null() { *out_size = need; }
        if data_size < need { return BAD_SIZE; }
        if !out.is_null() {
            std::ptr::copy_nonoverlapping(&val as *const T as *const u8, out as *mut u8, need as usize);
        }
    }
    NO_ERR
}

fn write_bytes_partial(out: *mut c_void, data_size: u32, out_size: *mut u32, bytes: &[u8]) -> OSStatus {
    // 数组属性：inDataSize 不足时截断返回（HAL 允许部分返回）
    unsafe {
        let write = bytes.len().min(data_size as usize);
        if !out_size.is_null() { *out_size = write as u32; }
        if !out.is_null() && write > 0 {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, write);
        }
    }
    NO_ERR
}

fn write_cfstring(out: *mut c_void, data_size: u32, out_size: *mut u32, s: &str) -> OSStatus {
    // 返回 +1 CFStringRef，宿主负责释放
    let cf = cf_string(s);
    unsafe {
        if !out_size.is_null() { *out_size = std::mem::size_of::<*mut c_void>() as u32; }
        if data_size < std::mem::size_of::<*mut c_void>() as u32 { return BAD_SIZE; }
        if !out.is_null() {
            std::ptr::copy_nonoverlapping(&cf as *const *mut c_void as *const u8, out as *mut u8, std::mem::size_of::<*mut c_void>());
        }
    }
    NO_ERR
}

// ---- 多设备辅助：对象类型判断 + 按设备取值 ----
fn is_dev(obj: AudioObjectID) -> bool { matches!(obj, DEV_A | DEV_B) }
fn is_stream_out(obj: AudioObjectID) -> bool { matches!(obj, A_OUT | B_OUT) }
fn is_stream_in(obj: AudioObjectID) -> bool { matches!(obj, A_IN | B_IN) }
fn is_stream(obj: AudioObjectID) -> bool { is_stream_out(obj) || is_stream_in(obj) }
fn is_vol(obj: AudioObjectID) -> bool { matches!(obj, A_VOL | B_VOL) }
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
    unsafe {
        let cf = *(qdata as *const *mut c_void);
        let mut buf = [0 as std::ffi::c_char; 128];
        if !cf.is_null() && CFStringGetCString(cf, buf.as_mut_ptr(), 128, UTF8) {
            let s = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy();
            for (i, meta) in DEVS.iter().enumerate() {
                if s.contains(meta.uid) { return if i == 0 { DEV_A } else { DEV_B }; }
            }
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
    let scope = unsafe { (*addr).m_scope };
    let ok = match obj {
        OBJ_PLUGIN => matches!(sel, SEL_PMFR | SEL_PNAM | SEL_PVER | SEL_PBOX | SEL_PDEV | SEL_UIDB | SEL_UIDD | SEL_RSRC | SEL_LNAM | SEL_LMOD | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND),
        OBJ_BOX => matches!(sel, SEL_BUID | SEL_BTRN | SEL_BHAU | SEL_BHVI | SEL_BHMI | SEL_BPRO | SEL_BXON | SEL_BXOF | SEL_BDV | SEL_BNAM | SEL_BMFR | SEL_BMOD | SEL_BSNO | SEL_BFMW | SEL_LNAM | SEL_LMOD | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_IDEN | SEL_SNUM | SEL_FWVN),
        DEV_A | DEV_B => matches!(sel, SEL_UID | SEL_MUID | SEL_TRAN | SEL_GROU | SEL_CLKD | SEL_LIVN | SEL_GOIN | SEL_GONE | SEL_DFLT | SEL_SFLT | SEL_LTNC | SEL_STM | SEL_CTRL | SEL_SAFT | SEL_NSRT | SEL_NSR | SEL_HIDN | SEL_FSIZ | SEL_FSZ | SEL_VFSZ | SEL_DCH2 | SEL_LNAM | SEL_LMOD | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_RING | SEL_CSTB | SEL_CLOK | SEL_ICON | SEL_SRND | SEL_VDSP | SEL_VRUT | SEL_CUST),
        A_OUT | A_IN | B_OUT | B_IN => matches!(sel, SEL_SACT | SEL_SDIR | SEL_TERM | SEL_SCHN | SEL_LTNC | SEL_SFMT | SEL_PFT | SEL_SFMA | SEL_PFTA | SEL_LNAM | SEL_LMAK | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND),
        A_VOL | B_VOL => matches!(sel, SEL_STBL | SEL_VLSC | SEL_VMIN | SEL_VMAX | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_CSCP | SEL_CELM | SEL_LCDV | SEL_LCDR),
        A_MUTE | B_MUTE => matches!(sel, SEL_STBL | SEL_MUTE | SEL_CLAS | SEL_BCLS | SEL_OWNE | SEL_OWND | SEL_CSCP | SEL_CELM),
        _ => false,
    };
    // 'stm#' 有 scope 限定（output 返回输出流，input 返回输入流）
    if is_dev(obj) && sel == SEL_STM && scope != SCOPE_OUTPUT && scope != SCOPE_INPUT {
        return 0;
    }
    ok as u8
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
    unsafe { *out = settable as u8; }
    NO_ERR
}

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
    let scope = unsafe { (*addr).m_scope };
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
            SEL_OWND => 4 * std::mem::size_of::<AudioObjectID>() as u32,
            SEL_UID | SEL_MUID | SEL_LNAM | SEL_LMOD | SEL_LMAK => std::mem::size_of::<*mut c_void>() as u32,
            SEL_TRAN | SEL_CLKD | SEL_DFLT | SEL_SFLT | SEL_LTNC | SEL_SAFT | SEL_HIDN | SEL_FSIZ | SEL_VFSZ | SEL_GOIN | SEL_GONE | SEL_LIVN => 4,
            SEL_GROU => std::mem::size_of::<AudioObjectID>() as u32,
            SEL_STM => {
                if scope == SCOPE_INPUT { 1 * std::mem::size_of::<AudioObjectID>() as u32 }
                else { 1 * std::mem::size_of::<AudioObjectID>() as u32 }
            }
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

unsafe extern "C" fn plugin_get_property_data(
    _driver: AudioServerPlugInDriverRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: *const AudioObjectPropertyAddress,
    _qsize: u32,
    _qdata: *const c_void,
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
                    std::slice::from_raw_parts(devs.as_ptr() as *const u8, 2 * std::mem::size_of::<AudioObjectID>())
                })
            }
            SEL_UIDB => write_out(out, data_size, out_size, OBJ_BOX),
            SEL_UIDD => write_out(out, data_size, out_size, translate_uid(_qdata)),
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
                    std::slice::from_raw_parts(devs.as_ptr() as *const u8, 2 * std::mem::size_of::<AudioObjectID>())
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
                    CustomPropertyInfo { m_selector: SEL_VDSP, m_data_type: 0x63667374 /* 'cfst' */, m_qualifier_data_type: 0 },
                    CustomPropertyInfo { m_selector: SEL_VRUT, m_data_type: 0x63667374 /* 'cfst' */, m_qualifier_data_type: 0 },
                ];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(info.as_ptr() as *const u8, 2 * std::mem::size_of::<CustomPropertyInfo>())
                })
            }
            SEL_VRUT => {
                let r = route().lock().unwrap_or_else(|e| e.into_inner());
                let s = format!("{:.1},{:.1},{:.1},{:.1}", r[0][0], r[0][1], r[1][0], r[1][1]);
                write_cfstring(out, data_size, out_size, &s)
            }
            SEL_VDSP => {
                let p = dsp().lock().unwrap_or_else(|e| e.into_inner()).params();
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
                    std::slice::from_raw_parts(layout.as_ptr() as *const u8, 12)
                })
            }
            SEL_ICON => write_cfstring(out, data_size, out_size, ""),
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
                let bytes = unsafe { std::slice::from_raw_parts(owned.as_ptr() as *const u8, owned.len() * 4) };
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
                    std::slice::from_raw_parts(rates.as_ptr() as *const u8, std::mem::size_of::<[AudioValueRange; 2]>())
                })
            }
            SEL_STM => {
                let stream = if scope == SCOPE_INPUT { dev_in(obj) } else { dev_out(obj) };
                write_out(out, data_size, out_size, stream)
            }
            SEL_CTRL => {
                let ctrls = [dev_vol(obj), dev_mute(obj)];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(ctrls.as_ptr() as *const u8, std::mem::size_of::<[AudioObjectID; 2]>())
                })
            }
            SEL_DCH2 => {
                let chs = [1u32, 2u32];
                write_bytes_partial(out, data_size, out_size, unsafe {
                    std::slice::from_raw_parts(chs.as_ptr() as *const u8, 8)
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
                SEL_SDIR => write_out(out, data_size, out_size, input as u32),
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
                        std::slice::from_raw_parts(fmts.as_ptr() as *const u8, std::mem::size_of::<[AudioStreamBasicDescription; 2]>())
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
                    std::slice::from_raw_parts(r.as_ptr() as *const u8, std::mem::size_of::<AudioValueRange>())
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
            let rate = unsafe { *(data as *const f64) };
            if rate != 44100.0 && rate != 48000.0 {
                return BAD_PROP;
            }
            SAMPLE_RATE.store(rate as u64, Ordering::SeqCst);
            NO_ERR
        }
        DEV_A | DEV_B if sel == SEL_FSIZ => NO_ERR, // 接受任意 buffer size
        DEV_A | DEV_B if sel == SEL_VDSP => {
            // data 是 CFStringRef："gain,low,mid,high"
            let cf = unsafe { *(data as *const *mut c_void) };
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
            dsp().lock().unwrap_or_else(|e| e.into_inner()).set_params(parts[0], parts[1], parts[2], parts[3], rate);
            NO_ERR
        }
        DEV_A | DEV_B if sel == SEL_VRUT => {
            let cf = unsafe { *(data as *const *mut c_void) };
            let mut buf = [0 as std::ffi::c_char; 128];
            if cf.is_null() || !unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), 128, UTF8) } {
                return BAD_PROP;
            }
            let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
            let parts: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if parts.len() != 4 {
                return BAD_PROP;
            }
            let mut r = route().lock().unwrap_or_else(|e| e.into_inner());
            r[0][0] = parts[0]; r[0][1] = parts[1];
            r[1][0] = parts[2]; r[1][1] = parts[3];
            NO_ERR
        }
        A_VOL | B_VOL if sel == SEL_VLSC => NO_ERR,
        A_MUTE | B_MUTE if sel == SEL_MUTE => NO_ERR,
        _ => BAD_PROP,
    }
}
