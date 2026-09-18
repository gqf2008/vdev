//
// macos-bluetooth-hfp-probe — 验证 macOS 能否作为 HFP Hands-Free unit（耳机）连接一台手机
//
// 背景与结论见 docs/dev/macos-bluetooth-role-survey.md（vdev issue：bt-role-survey-1）。
// 结论摘要：控制面可用（SLC/来电显示/拨号/通话状态），**通话音频拿不到**——
//           macOS 26 的 IOBluetoothHandsFreeDevice 已没有 SCO 音频桥。
//
// 行为分级（重要）：
//   只读、不改任何状态：--list / --sdp
//   会改状态（可逆，退出即断）：--hfp / --ag 会真的向目标手机发起 HFP 服务级连接；
//     --dial <号码> 会真的让手机拨出电话；--auto-accept 会真的接听来电；
//     --reset 会断开并重建与手机的蓝牙连接。
//
// 构建：见同目录 macos-bluetooth-role-probe.sh
//

#import <Foundation/Foundation.h>
#import <IOBluetooth/IOBluetooth.h>
#import <CoreAudio/CoreAudio.h>

// 未在公开头文件里声明、但运行时确实存在的私有选择器（只读用途）
@interface IOBluetoothDevice (PrivateProbe)
- (BOOL)isA2DPSink;
- (BOOL)isA2DPSource;
- (BOOL)isAudioSink;
- (AudioDeviceID)inputAudioDeviceID;
- (AudioDeviceID)outputAudioDeviceID;
@end

static NSDate *gT0 = nil;

static NSString *describeIOReturn(int status) {
    switch ((unsigned)status) {
        case 0x00000000: return @"kIOReturnSuccess";
        case 0xE00002BC: return @"kIOReturnError";
        case 0xE00002BE: return @"kIOReturnNoResources";
        case 0xE00002C0: return @"kIOReturnNoDevice";
        case 0xE00002C1: return @"kIOReturnNotPrivileged";
        case 0xE00002C2: return @"kIOReturnBadArgument";
        case 0xE00002C5: return @"kIOReturnExclusiveAccess";
        case 0xE00002C7: return @"kIOReturnUnsupported";
        default:         return [NSString stringWithFormat:@"0x%08X", (unsigned)status];
    }
}

static void LOG(NSString *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    NSString *s = [[NSString alloc] initWithFormat:fmt arguments:ap];
    va_end(ap);
    double t = [[NSDate date] timeIntervalSinceDate:gT0];
    printf("[%7.3fs] %s\n", t, [s UTF8String]);
    fflush(stdout);
}

// ---------- Class of Device 解码 ----------

// ---------- CoreAudio 端点枚举（看蓝牙音频端点何时出现） ----------

static AudioObjectPropertyAddress propAddr(AudioObjectPropertySelector sel,
                                           AudioObjectPropertyScope scope,
                                           AudioObjectPropertyElement elem) {
    AudioObjectPropertyAddress a = { sel, scope, elem };
    return a;
}

static NSString *deviceCFString(AudioObjectID dev, AudioObjectPropertySelector sel) {
    CFStringRef s = NULL;
    UInt32 size = sizeof(s);
    AudioObjectPropertyAddress a = propAddr(sel, kAudioObjectPropertyScopeGlobal,
                                           kAudioObjectPropertyElementMain);
    if (AudioObjectGetPropertyData(dev, &a, 0, NULL, &size, &s) != noErr || !s) return @"?";
    NSString *r = [NSString stringWithString:(__bridge NSString *)s];
    CFRelease(s);
    return r;
}

static UInt32 deviceUInt32(AudioObjectID dev, AudioObjectPropertySelector sel, UInt32 scope) {
    UInt32 v = 0, size = sizeof(v);
    AudioObjectPropertyAddress a = propAddr(sel, scope, kAudioObjectPropertyElementMain);
    if (AudioObjectGetPropertyData(dev, &a, 0, NULL, &size, &v) != noErr) return 0;
    return v;
}

static int channelCount(AudioObjectID dev, AudioObjectPropertyScope scope) {
    AudioObjectPropertyAddress a = propAddr(kAudioDevicePropertyStreamConfiguration, scope,
                                           kAudioObjectPropertyElementMain);
    UInt32 size = 0;
    if (AudioObjectGetPropertyDataSize(dev, &a, 0, NULL, &size) != noErr || size == 0) return -1;
    AudioBufferList *bl = (AudioBufferList *)malloc(size);
    if (!bl) return -1;
    int total = -1;
    if (AudioObjectGetPropertyData(dev, &a, 0, NULL, &size, bl) == noErr) {
        total = 0;
        for (UInt32 i = 0; i < bl->mNumberBuffers; i++) total += bl->mBuffers[i].mNumberChannels;
    }
    free(bl);
    return total;
}

static void dumpCoreAudioEndpoints(const char *tag) {
    AudioObjectPropertyAddress a = propAddr(kAudioHardwarePropertyDevices,
                                           kAudioObjectPropertyScopeGlobal,
                                           kAudioObjectPropertyElementMain);
    UInt32 size = 0;
    if (AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &a, 0, NULL, &size) != noErr) return;
    UInt32 n = size / sizeof(AudioDeviceID);
    AudioDeviceID *ids = (AudioDeviceID *)malloc(size);
    if (!ids) return;
    if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &a, 0, NULL, &size, ids) != noErr) {
        free(ids); return;
    }

    int btCount = 0;
    for (UInt32 i = 0; i < n; i++) {
        UInt32 t = deviceUInt32(ids[i], kAudioDevicePropertyTransportType,
                                kAudioObjectPropertyScopeGlobal);
        if (t == kAudioDeviceTransportTypeBluetooth || t == kAudioDeviceTransportTypeBluetoothLE) btCount++;
    }
    LOG(@"=== CoreAudio 端点%s —— 共 %u 个设备，其中蓝牙 %d 个 ===", tag, (unsigned)n, btCount);
    for (UInt32 i = 0; i < n; i++) {
        UInt32 transport = deviceUInt32(ids[i], kAudioDevicePropertyTransportType,
                                        kAudioObjectPropertyScopeGlobal);
        BOOL bt = (transport == kAudioDeviceTransportTypeBluetooth) ||
                  (transport == kAudioDeviceTransportTypeBluetoothLE);
        if (!bt) continue;
        double rate = 0; UInt32 rs = sizeof(rate);
        AudioObjectPropertyAddress ra = propAddr(kAudioDevicePropertyNominalSampleRate,
                                                 kAudioObjectPropertyScopeGlobal,
                                                 kAudioObjectPropertyElementMain);
        AudioObjectGetPropertyData(ids[i], &ra, 0, NULL, &rs, &rate);
        LOG(@"  id=%-4u  %-34s  in=%-3d out=%-3d  %.0f Hz",
            (unsigned)ids[i],
            [deviceCFString(ids[i], kAudioObjectPropertyName) UTF8String],
            channelCount(ids[i], kAudioDevicePropertyScopeInput),
            channelCount(ids[i], kAudioDevicePropertyScopeOutput),
            rate);
    }
    free(ids);
}

static NSString *majorClassName(uint32_t cod) {
    switch ((cod >> 8) & 0x1F) {
        case 0x00: return @"Miscellaneous";
        case 0x01: return @"Computer";
        case 0x02: return @"Phone";
        case 0x03: return @"LAN/Network";
        case 0x04: return @"Audio/Video";
        case 0x05: return @"Peripheral";
        case 0x06: return @"Imaging";
        case 0x07: return @"Wearable";
        case 0x08: return @"Toy";
        case 0x09: return @"Health";
        default:   return [NSString stringWithFormat:@"0x%X", (cod >> 8) & 0x1F];
    }
}

static NSString *minorClassName(uint32_t cod) {
    uint32_t major = (cod >> 8) & 0x1F;
    uint32_t minor = (cod >> 2) & 0x3F;

    if (major == 0x04) {
        switch (minor) {
            case 0x00: return @"Unspecified";
            case 0x01: return @"Wearable Headset";
            case 0x02: return @"Hands-free";
            case 0x04: return @"Microphone";
            case 0x05: return @"Loudspeaker";
            case 0x06: return @"Headphones";
            case 0x07: return @"Portable Audio";
            case 0x08: return @"Car audio";
            case 0x09: return @"Set-top box";
            case 0x0A: return @"Hi-Fi Audio Device";
            case 0x0B: return @"VCR";
            case 0x0C: return @"Video Camera";
            default:   return [NSString stringWithFormat:@"A/V minor 0x%X", minor];
        }
    }
    if (major == 0x02) {
        switch (minor) {
            case 0x00: return @"Uncategorized";
            case 0x01: return @"Cellular";
            case 0x02: return @"Cordless";
            case 0x03: return @"Smartphone";
            case 0x04: return @"Wired modem / voice gateway";
            case 0x05: return @"Common ISDN";
            default:   return [NSString stringWithFormat:@"Phone minor 0x%X", minor];
        }
    }
    return [NSString stringWithFormat:@"minor 0x%X", minor];
}

static BOOL looksLikePhone(uint32_t cod) {
    uint32_t major = (cod >> 8) & 0x1F;
    return major == 0x02 || major == 0x00;
}

// ---------- SDP 服务识别 ----------

typedef struct { uint16_t uuid; const char *name; } UUIDName;

static const UUIDName kUUIDs[] = {
    {0x1101, "SerialPort (SPP)"},
    {0x1108, "Headset (HSP-HS)"},
    {0x1109, "Cordless Telephony"},
    {0x110A, "A2DP **Source**"},
    {0x110B, "A2DP **Sink**"},
    {0x110C, "AVRCP Target"},
    {0x110D, "Advanced Audio Distribution"},
    {0x110E, "AVRCP Controller"},
    {0x1112, "Headset AG (HSP-AG)"},
    {0x111E, "Handsfree (HFP-**HF**)"},
    {0x111F, "Handsfree AG (HFP-**AG**)"},
    {0x112D, "SIM Access"},
    {0x112E, "Phonebook Access PCE"},
    {0x112F, "Phonebook Access PSE"},
    {0x1130, "Phonebook Access (PBAP)"},
    {0x1132, "Message Access Server (MAP)"},
    {0x1133, "Message Notification Server (MAP)"},
    {0x1124, "Human Interface Device"},
    {0x1800, "Generic Access"},
};

static NSString *serviceRoles(IOBluetoothSDPServiceRecord *rec) {
    NSMutableArray *hits = [NSMutableArray array];
    for (size_t i = 0; i < sizeof(kUUIDs) / sizeof(kUUIDs[0]); i++) {
        if ([rec matchesUUID16:kUUIDs[i].uuid]) {
            [hits addObject:[NSString stringWithUTF8String:kUUIDs[i].name]];
        }
    }
    return hits.count ? [hits componentsJoinedByString:@", "] : @"(未匹配已知 UUID)";
}

// ---------- 设备枚举 ----------

static IOBluetoothDevice *findDevice(NSString *needle) {
    NSArray *devs = [IOBluetoothDevice pairedDevices];
    for (IOBluetoothDevice *d in devs) {
        NSString *addr = [[d addressString] stringByReplacingOccurrencesOfString:@"-" withString:@":"];
        NSString *name = [d name] ?: @"";
        if ([[addr lowercaseString] isEqualToString:[needle lowercaseString]] ||
            [name.lowercaseString containsString:needle.lowercaseString]) {
            return d;
        }
    }
    return nil;
}

static void dumpDevice(IOBluetoothDevice *d, BOOL verbose) {
    uint32_t cod = (uint32_t)[d classOfDevice];
    printf("\n───────────────────────────────────────────────────────────────\n");
    printf("  name        : %s\n", [[d name] ?: @"(nil)" UTF8String]);
    printf("  address     : %s\n", [[[d addressString] stringByReplacingOccurrencesOfString:@"-"
                                   withString:@":"] UTF8String]);
    printf("  CoD         : 0x%08X  →  %s / %s\n", cod,
           [majorClassName(cod) UTF8String], [minorClassName(cod) UTF8String]);
    printf("  paired/conn : %d / %d\n", [d isPaired], [d isConnected]);

    if (verbose) {
        printf("  --- 角色探测（私有属性，只读） ---\n");
        printf("  isHandsFreeAudioGateway (对端是手机 AG?) : %d\n", [d isHandsFreeAudioGateway]);
        printf("  isHandsFreeDevice       (对端是耳机 HF?) : %d\n", [d isHandsFreeDevice]);
        printf("  isA2DPSink              (对端是音箱?)    : %d\n", [d isA2DPSink]);
        printf("  isA2DPSource            (对端是音源?)    : %d\n", [d isA2DPSource]);
        printf("  isAudioSink                              : %d\n", [d isAudioSink]);
        printf("  inputAudioDeviceID / outputAudioDeviceID : %u / %u\n",
               (unsigned)[d inputAudioDeviceID], (unsigned)[d outputAudioDeviceID]);
        IOBluetoothSDPServiceRecord *ag = [d handsFreeAudioGatewayServiceRecord];
        printf("  HFP-AG SDP 记录存在?                     : %s\n", ag ? "YES" : "no");

        NSArray *services = [d services];
        if (!services) {
            printf("  --- SDP: 无缓存（先用 --sdp 查询一次） ---\n");
        } else {
            printf("  --- SDP 服务 (%lu 条) ---\n", (unsigned long)services.count);
            for (IOBluetoothSDPServiceRecord *rec in services) {
                printf("    · %-34s  %s\n",
                       [[rec getServiceName] ?: @"(unnamed)" UTF8String],
                       [serviceRoles(rec) UTF8String]);
            }
        }
    }
}

// ---------- HFP 连接探针 ----------

@interface Probe : NSObject <IOBluetoothHandsFreeDeviceDelegate, IOBluetoothHandsFreeAudioGatewayDelegate>
@property (strong) IOBluetoothHandsFreeDevice *hf;
@property (strong) IOBluetoothHandsFreeAudioGateway *ag;
@property (strong) IOBluetoothDevice *dev;
@property (assign) BOOL forceSCO;      // --sco: SLC 建好后强制拉起 SCO 语音链路
@property (assign) BOOL verboseAudio;  // --audio: 打印 CoreAudio 蓝牙端点
@property (copy)   NSString *dialNumber;   // --dial <号码>: 从 Mac 侧拨号（ATD）
@property (assign) BOOL autoAccept;        // --auto-accept: 来电时自动从 Mac 接听
@property (assign) BOOL transferAudio;     // --transfer: 通话接通后要求把音频转到 Mac
@property (copy)   NSString *lastState;    // 状态变化检测
@property (assign) int tick;
@property (assign) BOOL didDial;           // 防止看门狗重连后重复拨号
@property (assign) BOOL didRequestSCO;
@property (strong) NSTimer *transferTimer; // 通话期间反复尝试把音频转到 Mac
@property (assign) int transferAttempts;
@property (assign) int scoCallbackCount;   // SCO 回调计数（证据可复核：与重试序号成对出现）
@end

@implementation Probe

- (instancetype)initWithDevice:(IOBluetoothDevice *)dev {
    if ((self = [super init])) { _dev = dev; }
    return self;
}

- (void)startStatusTimer {
    NSTimer *t = [NSTimer scheduledTimerWithTimeInterval:2.0 repeats:YES block:^(NSTimer *timer) {
        IOBluetoothHandsFree *obj = (IOBluetoothHandsFree *)self.hf ?: (IOBluetoothHandsFree *)self.ag;
        if (!obj) return;
        NSString *state = [NSString stringWithFormat:
            @"SLC=%d SCO=%d service=%d call=%d callsetup=%d signal=%d batt=%d audio=in%u/out%u",
            [obj isConnected], [obj isSCOConnected],
            [obj indicator:@"service"], [obj indicator:@"call"],
            [obj indicator:@"callsetup"], [obj indicator:@"signal"],
            [obj indicator:@"battchg"],
            (unsigned)[self.dev inputAudioDeviceID], (unsigned)[self.dev outputAudioDeviceID]];
        // 只在状态变化时打印（另每 10 秒打一次心跳），避免 2 秒一行刷屏
        if (![state isEqualToString:self.lastState]) {
            LOG(@"status 变化: %@  →  %@", self.lastState ?: @"(首次)", state);
            self.lastState = state;
            if (self.verboseAudio) dumpCoreAudioEndpoints(" (状态变化)");
        } else if (self.tick % 5 == 0) {
            LOG(@"status 心跳: %@", state);
        }
        self.tick++;
    }];
    [[NSRunLoop currentRunLoop] addTimer:t forMode:NSDefaultRunLoopMode];
}

- (BOOL)connectAsHandsFree {
    LOG(@"创建 IOBluetoothHandsFreeDevice（Mac = HF 耳机侧），目标手机 = %@", [self.dev name]);
    self.hf = [[IOBluetoothHandsFreeDevice alloc] initWithDevice:self.dev delegate:self];
    if (!self.hf) { LOG(@"❌ initWithDevice 返回 nil —— API 不可用"); return NO; }

    LOG(@"本地 HF 支持特性 supportedFeatures = 0x%08X", [self.hf supportedFeatures]);
    LOG(@"对端手机支持特性 deviceSupportedFeatures = 0x%08X", [self.hf deviceSupportedFeatures]);
    LOG(@"正在发起 HFP 服务级连接 connect() ...");
    [self.hf connect];
    [self startSLCWatchdog];

    // 注意：--dial / --sco 不在 connect() 之后按墙钟计时触发，而是等 SLC 真正建立后
    // 再发（见 handsFree:connected:）。否则 SLC 晚到时 AT 命令会静默丢失。
    return YES;
}

// SLC 看门狗：12 秒还没 connected 就报诊断 + 自动重试一次，再 12 秒仍不行就给出恢复步骤
- (void)startSLCWatchdog {
    __block int attempt = 0;
    NSTimer *t = [NSTimer scheduledTimerWithTimeInterval:12.0 repeats:YES block:^(NSTimer *timer) {
        if ([self.hf isConnected]) { [timer invalidate]; return; }
        attempt++;
        if (attempt == 1) {
            LOG(@"⚠️ 已等 12 秒仍未建立 SLC。常见原因：手机侧残留上一次 HFP 会话 / 手机正在通话 / 蓝牙需重置。");
            LOG(@"   自动重试一次：disconnect → 等 1.5 秒 → connect");
            [self.hf disconnect];
            [NSTimer scheduledTimerWithTimeInterval:1.5 repeats:NO block:^(NSTimer *t2) {
                LOG(@"   重试 connect() ...");
                [self.hf connect];
            }];
        } else {
            LOG(@"⚠️ 重试后仍未建立 SLC（累计约 %d 秒）。建议按顺序做：", attempt * 12);
            LOG(@"     1) 手机上：蓝牙关 → 开（清掉残留的耳机会话）");
            LOG(@"     2) 确认手机 设置→蓝牙 里本机那台没有显示「已连接」");
            LOG(@"     3) 仍然不行就把 Mac 蓝牙也关→开，再重跑本命令");
            LOG(@"   也可以加 --reset 让探针先断开并重建与手机的蓝牙连接。");
            if (self.dialNumber.length && !self.didDial) {
                LOG(@"   ⚠️ 因为 SLC 没建立，--dial 的拨号没有发出（不会产生通话）。");
            }
            [timer invalidate];
        }
    }];
    [[NSRunLoop currentRunLoop] addTimer:t forMode:NSDefaultRunLoopMode];
}

- (BOOL)connectAsAudioGateway {
    LOG(@"创建 IOBluetoothHandsFreeAudioGateway（Mac = AG 手机侧），目标 = %@", [self.dev name]);
    self.ag = [[IOBluetoothHandsFreeAudioGateway alloc] initWithDevice:self.dev delegate:self];
    if (!self.ag) { LOG(@"❌ initWithDevice 返回 nil"); return NO; }
    LOG(@"正在发起 HFP AG 连接 connect() ...");
    [self.ag connect];
    return YES;
}

// ---- IOBluetoothHandsFreeDelegate（基类） ----
- (void)handsFree:(IOBluetoothHandsFree *)device connected:(NSNumber *)status {
    LOG(@"✅ handsFree:connected: status=%@  ← SLC 已建立，可以发 AT 命令了", status);
    self.lastState = nil;

    // SLC 建立后才允许发 AT 命令：强开 SCO / 拨号
    if (self.forceSCO && !self.didRequestSCO) {
        self.didRequestSCO = YES;
        [NSTimer scheduledTimerWithTimeInterval:0.8 repeats:NO block:^(NSTimer *t) {
            LOG(@"⏫ --sco: 调用 connectSCO 拉起语音链路 ...");
            [self.hf connectSCO];
        }];
    }
    if (self.dialNumber.length && !self.didDial) {
        self.didDial = YES;
        [NSTimer scheduledTimerWithTimeInterval:0.8 repeats:NO block:^(NSTimer *t) {
            LOG(@"📞 --dial: 从 Mac 侧拨号 %@（ATD）—— 手机会真的打出去", self.dialNumber);
            [self.hf dialNumber:self.dialNumber];
        }];
    }
}
- (void)handsFree:(IOBluetoothHandsFree *)device disconnected:(NSNumber *)status {
    LOG(@"⛔️ handsFree:disconnected: status=%@", status);
}
- (void)handsFree:(IOBluetoothHandsFree *)device scoConnectionOpened:(NSNumber *)status {
    int s = [status intValue];
    self.scoCallbackCount++;
    if (s == 0) {
        LOG(@"🎧 [SCO 回调 #%d] 语音链路已建立（status=0）← 通话音频应开始流向 Mac",
            self.scoCallbackCount);
    } else {
        LOG(@"❌ [SCO 回调 #%d] 打开失败：status=%d (%@)",
            self.scoCallbackCount, s, describeIOReturn(s));
    }
    if (self.verboseAudio) dumpCoreAudioEndpoints(" (SCO 打开后)");
    self.lastState = nil;   // 强制下一拍打印状态
}
- (void)handsFree:(IOBluetoothHandsFree *)device scoConnectionClosed:(NSNumber *)status {
    LOG(@"🔇 SCO 关闭：status=%@ (%@)", status, describeIOReturn([status intValue]));
    self.lastState = nil;
}

// ---- IOBluetoothHandsFreeDeviceDelegate（HF 侧） ----
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d isServiceAvailable:(NSNumber *)v { LOG(@"  · 服务可用 = %@", v); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d isCallActive:(NSNumber *)v {
    LOG(@"  · 通话中 = %@", v);
    if ([v intValue] != 0) {
        LOG(@"📞📞 通话已接通 —— 这是 SCO 应当被 AG 拉起的时刻");
        if (self.transferAudio) [self startTransferRetry];
        if (self.verboseAudio) dumpCoreAudioEndpoints(" (通话接通)");
    } else {
        [self stopTransferRetry];
    }
    self.lastState = nil;
}

// 通话期间每 3 秒重试一次 transferAudioToComputer，直到 SCO 连上或通话结束。
// 单独隔离这个调用（不再叠加 connectSCO），避免两个请求互相竞争。
- (void)startTransferRetry {
    if (self.transferTimer) return;
    self.transferAttempts = 0;
    LOG(@"   开始每 3 秒重试 transferAudioToComputer（观察 SCO 是否被拉起）");
    self.transferTimer = [NSTimer scheduledTimerWithTimeInterval:3.0 repeats:YES block:^(NSTimer *t) {
        if ([self.hf isSCOConnected]) {
            LOG(@"🎧 SCO 已连接！audio=in%u/out%u",
                (unsigned)[self.dev inputAudioDeviceID], (unsigned)[self.dev outputAudioDeviceID]);
            if (self.verboseAudio) dumpCoreAudioEndpoints(" (SCO 已连)");
            [self stopTransferRetry];
            return;
        }
        self.transferAttempts++;
        // 先打"即将发起"，再调用：`transferAudioToComputer` 在本机是**同步**触发
        // `scoConnectionOpened:` 回调的，所以日志里紧随其后的 `[SCO 回调 #N]`
        // 就是本次请求的结果。不要用 `回调计数 + 1` 去"预测"编号，那样会差 1。
        LOG(@"   → 发起 transferAudioToComputer 第 %d 次（void 无返回值，结果见紧随的 [SCO 回调 #N]）"
            @" | 本次调用前 isSCOConnected=%d | audio=in%u/out%u",
            self.transferAttempts, [self.hf isSCOConnected],
            (unsigned)[self.dev inputAudioDeviceID], (unsigned)[self.dev outputAudioDeviceID]);
        [self.hf transferAudioToComputer];
    }];
    [[NSRunLoop currentRunLoop] addTimer:self.transferTimer forMode:NSDefaultRunLoopMode];
}

- (void)stopTransferRetry {
    if (!self.transferTimer) return;
    [self.transferTimer invalidate];
    self.transferTimer = nil;
    LOG(@"   通话结束，停止音频转移重试（共尝试 %d 次）", self.transferAttempts);
}
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d callSetupMode:(NSNumber *)v {
    LOG(@"  · 呼叫建立状态 = %@  (0=空闲 1=来电 2=拨出 3=回铃)", v);
    self.lastState = nil;
}
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d callHoldState:(NSNumber *)v { LOG(@"  · 呼叫保持 = %@", v); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d signalStrength:(NSNumber *)v { LOG(@"  · 信号 = %@", v); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d isRoaming:(NSNumber *)v { LOG(@"  · 漫游 = %@", v); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d batteryCharge:(NSNumber *)v { LOG(@"  · 电量 = %@", v); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d incomingCallFrom:(NSString *)n {
    LOG(@"📞 来电：%@", n);
    if (self.autoAccept) {
        LOG(@"   --auto-accept: 从 Mac 侧接听 acceptCall（这正是 Phone Amego 的流程）");
        [d acceptCall];
    }
}
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d ringAttempt:(NSNumber *)v { LOG(@"  · 振铃 %@", v); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d currentCall:(NSDictionary *)c { LOG(@"  · 当前通话 = %@", c); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d subscriberNumber:(NSString *)n { LOG(@"  · 本机号码 = %@", n); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d incomingSMS:(NSDictionary *)s { LOG(@"💬 收到短信 = %@", s); }
- (void)handsFree:(IOBluetoothHandsFreeDevice *)d unhandledResultCode:(NSString *)r { LOG(@"  · 未处理结果码 = %@", r); }

// ---- IOBluetoothHandsFreeAudioGatewayDelegate（AG 侧） ----
- (void)handsFree:(IOBluetoothHandsFreeAudioGateway *)d indicatorEvent:(NSDictionary *)e { LOG(@"  · AG indicator = %@", e); }

@end

// ---------- main ----------

static void usage(void) {
    printf(
        "用法:\n"
        "  hfp-probe --list              列出已配对设备 + CoD + SDP 角色（只读）\n"
        "  hfp-probe --sdp <地址|名字>   对该设备发起 SDP 查询并打印服务（只读）\n"
        "  hfp-probe --hfp <地址|名字>   以 HF（耳机）身份连接，观察 45 秒（会短暂占用手机的耳机通道）\n"
        "  hfp-probe --ag  <地址|名字>   以 AG（手机）身份连接，观察 45 秒\n"
        "  --seconds N                  观察时长（默认 45）\n"
        "  --sco                        连接后强制拉起 SCO 语音链路（不需要真的打电话）\n"
        "  --audio                      打印蓝牙 CoreAudio 端点，看音频落在哪里\n"
        "  --dial <号码>                从 Mac 侧拨号（ATD）——会真的打出电话，慎用\n"
        "  --auto-accept                来电时自动从 Mac 接听（Phone Amego 的接听流程）\n"
        "  --transfer                   通话接通后调用 transferAudioToComputer\n"
        "  --reset                      先断开并重建与手机的蓝牙连接（清残留 profile 会话）\n");
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        gT0 = [NSDate date];

        NSString *mode = nil, *target = nil;
        int seconds = 45;
        BOOL forceSCO = NO, verboseAudio = NO;
        NSString *dial = nil;
        BOOL autoAccept = NO, transfer = NO, resetFirst = NO;
        for (int i = 1; i < argc; i++) {
            NSString *a = [NSString stringWithUTF8String:argv[i]];
            if ([a isEqualToString:@"--list"]) mode = @"list";
            else if ([a isEqualToString:@"--sdp"]) { mode = @"sdp"; if (i + 1 < argc) target = [NSString stringWithUTF8String:argv[++i]]; }
            else if ([a isEqualToString:@"--hfp"]) { mode = @"hfp"; if (i + 1 < argc) target = [NSString stringWithUTF8String:argv[++i]]; }
            else if ([a isEqualToString:@"--ag"])  { mode = @"ag";  if (i + 1 < argc) target = [NSString stringWithUTF8String:argv[++i]]; }
            else if ([a isEqualToString:@"--seconds"]) { if (i + 1 < argc) seconds = atoi(argv[++i]); }
            else if ([a isEqualToString:@"--dial"]) { if (i + 1 < argc) dial = [NSString stringWithUTF8String:argv[++i]]; }
            else if ([a isEqualToString:@"--auto-accept"]) autoAccept = YES;
            else if ([a isEqualToString:@"--transfer"]) transfer = YES;
            else if ([a isEqualToString:@"--reset"]) resetFirst = YES;
            else if ([a isEqualToString:@"--sco"]) forceSCO = YES;
            else if ([a isEqualToString:@"--audio"]) verboseAudio = YES;
            else if ([a isEqualToString:@"-h"] || [a isEqualToString:@"--help"]) { usage(); return 0; }
        }
        if (!mode) { usage(); return 2; }

        printf("=== macos-bluetooth-hfp-probe @ macOS %s ===\n",
               [[[NSProcessInfo processInfo] operatingSystemVersionString] UTF8String]);

        if ([mode isEqualToString:@"list"]) {
            NSArray *devs = [IOBluetoothDevice pairedDevices];
            // `pairedDevices` 可能对同一台设备返回多个实例，按地址去重后再逐一探测
            NSMutableArray *uniq = [NSMutableArray array];
            NSMutableSet *seen = [NSMutableSet set];
            for (IOBluetoothDevice *d in devs) {
                NSString *k = [[d addressString] lowercaseString];
                if (!k || [seen containsObject:k]) continue;
                [seen addObject:k];
                [uniq addObject:d];
            }
            printf("\n已配对设备 %lu 台（按地址去重后 %lu 台）：\n",
                   (unsigned long)devs.count, (unsigned long)uniq.count);
            for (IOBluetoothDevice *d in uniq) dumpDevice(d, YES);

            BOOL foundPhone = NO;
            for (IOBluetoothDevice *d in uniq) {
                if ([d isHandsFreeAudioGateway] || looksLikePhone((uint32_t)[d classOfDevice])) { foundPhone = YES; }
            }
            printf("\n>>> 结论: %s\n", foundPhone
                   ? "发现疑似手机（CoD=Phone 或带 HFP-AG），可以跑 --hfp <地址>"
                   : "没有发现手机。请先用系统蓝牙面板把手机和这台 Mac 配对，再重跑 --list");
            return 0;
        }

        if (verboseAudio) dumpCoreAudioEndpoints("（连接前基线）");

        IOBluetoothDevice *dev = target ? findDevice(target) : nil;
        if (!dev) {
            printf("找不到设备: %s\n", [target UTF8String] ?: "(未指定)");
            return 1;
        }
        dumpDevice(dev, YES);

        if (resetFirst) {
            LOG(@"--reset: 断开与「%@」的蓝牙连接，重建以清理残留的 profile 会话 ...", [dev name]);
            [dev closeConnection];
            [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:2.5]];
            LOG(@"   重新 openConnection ...");
            [dev openConnection];
            [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:1.5]];
            LOG(@"   ACL 重连完成（connected=%d）", [dev isConnected]);
        }

        if ([mode isEqualToString:@"sdp"]) {
            LOG(@"发起 SDP 查询 ...");
            [dev performSDPQuery:nil];
            [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:15]];
            LOG(@"查询后重新打印：");
            dumpDevice(dev, YES);
            return 0;
        }

        Probe *probe = [[Probe alloc] initWithDevice:dev];
        probe.forceSCO = forceSCO;
        probe.verboseAudio = verboseAudio;
        probe.dialNumber = dial;
        probe.autoAccept = autoAccept;
        probe.transferAudio = transfer;
        [probe startStatusTimer];

        BOOL ok = [mode isEqualToString:@"hfp"] ? [probe connectAsHandsFree] : [probe connectAsAudioGateway];
        if (!ok) return 3;

        LOG(@"观察 %d 秒（Ctrl-C 可提前退出）...", seconds);
        NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:seconds];
        while ([deadline timeIntervalSinceNow] > 0) {
            [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                     beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.25]];
        }

        LOG(@"收尾：断开连接");
        if (probe.hf) [probe.hf disconnect];
        if (probe.ag) [probe.ag disconnect];
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:1.0]];
        if (verboseAudio) dumpCoreAudioEndpoints("（断开后）");
        LOG(@"结束。");
    }
    return 0;
}
