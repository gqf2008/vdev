//
// macos-bluetooth-hci-probe — 绕过 IOBluetoothHandsFreeDevice，直接用
//              IOBluetoothHostController 的私有 HCI 方法尝试建立 SCO/eSCO 链路。
//
// 背景与结论见 docs/dev/macos-bluetooth-role-survey.md（vdev issue：bt-role-survey-1）。
//
// 目的：macOS 的 HF 类在 macOS 26 上已删掉 SCO 音频桥（ivar _scoAudioDevice 不存在），
//       所以走类接口只会拿到 kIOReturnUnsupported。本工具改走**控制器层**：
//       自己发 Setup Synchronous Connection，看链路层到底能不能建起来。
//
// 方法论：先做阳性对照（Read Voice Setting，纯读），证明 HCI 通路可用了，
//         再去解释后面失败的含义。
//
// 实测结论（macOS 26.5.2）：阳性对照通过（用户态无需 root 即可发 HCI 命令），
//   但 `connectionHandle`/`getConnectionHandle` 恒为 0、`getLinkType`=255，
//   拿不到句柄就无法构造指向具体链路的 SCO 命令；且即便建起来也没有音频桥，
//   故这条路不再投入。
//
// 行为：只发一条只读命令（Read Voice Setting）+ 若干 Setup Synchronous Connection。
//       不做持久改动；若句柄为 0，Setup 会返回 success 却什么都不做（假阳性，
//       本工具以 out.connectionHandle != 0 作为真正的成功判据）。
//
// 构建：见同目录 macos-bluetooth-role-probe.sh
//

#import <Foundation/Foundation.h>
#import <IOBluetooth/IOBluetooth.h>

// 本探针**有意**调用已废弃的 API（`getConnectionHandle` / `getAddressString` 等）：
// 它们是否还能拿到值，本身就是"这条能力还在不在"的证据之一（实测 macOS 26 上恒为 0）。
// 因此在此显式关闭该告警，避免噪声掩盖真正的问题。
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

// ---- 私有 API（签名取自 colemancda/HCITool 的 class-dump 头 + 本机运行时核对）----
@interface IOBluetoothHostController (PrivateHCI)
- (unsigned int)classOfDevice;
- (int)BluetoothHCIReadVoiceSetting:(unsigned short *)arg1;
- (int)BluetoothHCIWriteVoiceSetting:(unsigned short)arg1;
- (int)BluetoothHCISetupSynchronousConnection:(unsigned short)arg1
                           inTransmitBandwidth:(unsigned int)arg2
                            inReceiveBandwidth:(unsigned int)arg3
                                  inMaxLatency:(unsigned short)arg4
                                inVoiceSetting:(unsigned short)arg5
                       inRetransmissionEffort:(unsigned char)arg6
                                 inPacketType:(unsigned short)arg7
       outSynchronousConnectionCompleteResults:(BluetoothHCIEventSynchronousConnectionCompleteResults *)arg8;
@end

// 句柄相关的几个 getter（公开的 connectionHandle 在 macOS 26 上实测恒为 0，先都试一遍）
@interface IOBluetoothDevice (HandleProbe)
- (BluetoothConnectionHandle)getConnectionHandle;
- (uint8_t)getLinkType;
- (uint8_t)getEncryptionMode;
@end

static BluetoothConnectionHandle resolveHandle(IOBluetoothDevice *dev) {
    BluetoothConnectionHandle h = [dev connectionHandle];
    printf("  [dev connectionHandle]    = 0x%04X\n", h);
    if (h) return h;

    if ([dev respondsToSelector:@selector(getConnectionHandle)]) {
        BluetoothConnectionHandle h2 = [dev getConnectionHandle];
        printf("  [dev getConnectionHandle] = 0x%04X\n", h2);
        if (h2) return h2;
    } else {
        printf("  [dev getConnectionHandle] 不存在\n");
    }
    return 0;
}

static NSString *describeIOReturn(int s) {
    switch ((unsigned)s) {
        case 0x00000000: return @"kIOReturnSuccess";
        case 0xE00002BC: return @"kIOReturnError";
        case 0xE00002BE: return @"kIOReturnNoResources";
        case 0xE00002C0: return @"kIOReturnNoDevice";
        case 0xE00002C1: return @"kIOReturnNotPrivileged";
        case 0xE00002C2: return @"kIOReturnBadArgument";
        case 0xE00002C5: return @"kIOReturnExclusiveAccess";
        case 0xE00002C7: return @"kIOReturnUnsupported";
        case 0xE00002F0: return @"kIOReturnTimeout";
        default:         return [NSString stringWithFormat:@"0x%08X", (unsigned)s];
    }
}

static NSString *describeHCIError(uint8_t e) {
    switch (e) {
        case 0x00: return @"Success";
        case 0x01: return @"Unknown HCI Command";
        case 0x03: return @"Hardware Failure";
        case 0x05: return @"Authentication Failure";
        case 0x0C: return @"Command Disallowed";
        case 0x0F: return @"Connection Timeout (page timeout)";
        case 0x10: return @"Host Timeout  ← 2011 年 Phone Amego 那个内核日志就是这个";
        case 0x11: return @"Unsupported Feature or Parameter Value";
        case 0x12: return @"Invalid HCI Command Parameters";
        case 0x13: return @"Remote User Terminated Connection";
        case 0x16: return @"Connection Terminated By Local Host";
        case 0x1C: return @"Unsupported Remote Feature";
        case 0x1D: return @"SCO Air Mode Rejected";
        case 0x1E: return @"Invalid LMP Parameters";
        case 0x1F: return @"Unspecified Error";
        case 0x20: return @"Unsupported LMP Parameter Value";
        case 0x22: return @"LMP Response Timeout";
        case 0x23: return @"LMP Error Transaction Collision";
        case 0x2B: return @"Connection Rejected due to Security Reasons";
        case 0x3D: return @"Controller Busy";
        default:   return [NSString stringWithFormat:@"0x%02X", e];
    }
}

static NSString *voiceSettingDesc(uint16_t v) {
    NSArray *coding = @[@"Linear PCM", @"µ-law", @"A-law", @"reserved"];
    NSArray *air    = @[@"µ-law", @"A-law", @"CVSD", @"Transparent"];
    return [NSString stringWithFormat:
        @"inputCoding=%@ dataFormat=%@ sampleSize=%@bit pcmBitPos=%u airCoding=%@",
        coding[v & 0x3],
        (v & 0x4) ? @"2's complement" : @"1's complement",
        (v & 0x8) ? @"16" : @"8",
        ((v >> 4) & 0x7),
        air[(v >> 7) & 0x3]];
}

static BOOL parseAddress(NSString *s, BluetoothDeviceAddress *out) {
    NSArray *parts = [s componentsSeparatedByCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@":-"]];
    if (parts.count != 6) return NO;
    for (int i = 0; i < 6; i++) {
        unsigned v = 0;
        if (![[NSScanner scannerWithString:parts[i]] scanHexInt:&v]) return NO;
        out->data[i] = (uint8_t)v;
    }
    return YES;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        NSString *addrStr = nil;
        NSMutableArray *packetTypes = [NSMutableArray array];
        for (int i = 1; i < argc; i++) {
            NSString *a = [NSString stringWithUTF8String:argv[i]];
            if ([a isEqualToString:@"--addr"] && i + 1 < argc) addrStr = [NSString stringWithUTF8String:argv[++i]];
            else if ([a isEqualToString:@"--packet-type"] && i + 1 < argc) {
                unsigned v = 0;
                [[NSScanner scannerWithString:[NSString stringWithUTF8String:argv[++i]]] scanHexInt:&v];
                [packetTypes addObject:@(v)];
            }
        }
        if (!addrStr) {
            printf("用法: hci-probe --addr <aa:bb:cc:dd:ee:ff> [--packet-type 0x38] [--packet-type 0x8]\n");
            printf("  packet-type 位掩码: HV1=0x1 HV2=0x2 HV3=0x4 EV3=0x8 2-EV3=0x10 3-EV3=0x20 2-EV5=0x40 3-EV5=0x80\n");
            printf("  默认依次尝试 0x38 (EV3+2EV3+3EV3) 和 0x8 (EV3)\n");
            return 2;
        }
        if (packetTypes.count == 0) { packetTypes = [@[@0x38, @0x08] mutableCopy]; }

        printf("=== macos-bluetooth-hci-probe @ %s ===\n",
               [[[NSProcessInfo processInfo] operatingSystemVersionString] UTF8String]);

        IOBluetoothHostController *hc = [IOBluetoothHostController defaultController];
        if (!hc) { printf("❌ 拿不到 defaultController\n"); return 1; }
        printf("控制器 : %s  (%s)\n", [[hc nameAsString] UTF8String], [[hc addressAsString] UTF8String]);
        printf("powerState=%d  classOfDevice=0x%08X\n", [hc powerState], [hc classOfDevice]);

        // ---------- 阳性对照：纯读，证明 HCI 通路可用 ----------
        printf("\n--- 阳性对照：BluetoothHCIReadVoiceSetting（只读） ---\n");
        if (![hc respondsToSelector:@selector(BluetoothHCIReadVoiceSetting:)]) {
            printf("❌ 私有方法不存在，这条路走不通\n");
            return 3;
        }
        unsigned short vs = 0;
        int rc = [hc BluetoothHCIReadVoiceSetting:&vs];
        printf("返回 %d (%s)", rc, [describeIOReturn(rc) UTF8String]);
        if (rc == 0) {
            printf("   voiceSetting=0x%04X  [%s]\n", vs, [voiceSettingDesc(vs) UTF8String]);
            printf("✅ HCI 通路可用（这就是阳性对照；没有它，后面的失败无法解释）\n");
        } else {
            printf("\n⚠️ 读都失败了——说明用户态 HCI 通路本身不通，后面无需再试\n");
            return 4;
        }

        // ---------- 目标设备 + 连接句柄 ----------
        IOBluetoothDevice *dev = [IOBluetoothDevice deviceWithAddressString:addrStr];
        if (!dev) { printf("❌ 找不到设备 %s\n", [addrStr UTF8String]); return 1; }
        printf("\n--- 目标 ---\n");
        printf("设备   : %s (%s)\n", [[dev name] UTF8String], [[dev getAddressString] UTF8String]);
        printf("已连接 : %d\n", [dev isConnected]);
        BluetoothConnectionHandle handle = resolveHandle(dev);
        if ([dev respondsToSelector:@selector(getLinkType)])
            printf("  [dev getLinkType]         = %u   getEncryptionMode = %u\n",
                   [dev getLinkType], [dev getEncryptionMode]);
        if (![dev isConnected] || handle == 0) {
            printf("⚠️ 没有 ACL 基带连接，或所有 getter 都拿不到句柄 —— 需要先建立连接"
                   "（可先跑 hfpprobe --hfp 一次），且必须拿到非零句柄才能发 SCO 命令\n");
        }

        BluetoothDeviceAddress da;
        if (!parseAddress(addrStr, &da)) { printf("❌ 地址解析失败\n"); return 1; }
        printf("地址字节: %02X %02X %02X %02X %02X %02X\n",
               da.data[0], da.data[1], da.data[2], da.data[3], da.data[4], da.data[5]);

        // ---------- 实验：Setup Synchronous Connection ----------
        for (NSNumber *ptNum in packetTypes) {
            unsigned short pt = (unsigned short)[ptNum unsignedIntValue];
            BluetoothHCIEventSynchronousConnectionCompleteResults res;
            memset(&res, 0, sizeof(res));

            printf("\n--- BluetoothHCISetupSynchronousConnection (packetType=0x%04X) ---\n", pt);
            printf("参数: handle=0x%04X txBW=8000 rxBW=8000 maxLatency=0xFFFF voiceSetting=0x%04X "
                   "retxEffort=0xFF packetType=0x%04X\n",
                   handle, vs, pt);

            rc = [hc BluetoothHCISetupSynchronousConnection:handle
                                         inTransmitBandwidth:8000
                                          inReceiveBandwidth:8000
                                                inMaxLatency:0xFFFF
                                              inVoiceSetting:vs
                                     inRetransmissionEffort:0xFF
                                               inPacketType:pt
                     outSynchronousConnectionCompleteResults:&res];

            printf("返回 %d (%s)\n", rc, [describeIOReturn(rc) UTF8String]);
            printf("out: handle=0x%04X linkType=%u txInterval=%u retxWindow=%u "
                   "rxPktLen=%u txPktLen=%u airMode=%u (%s)\n",
                   res.connectionHandle, res.linkType, res.transmissionInterval,
                   res.retransmissionWindow, res.receivePacketLength,
                   res.transmitPacketLength, res.airMode,
                   res.airMode == 2 ? "CVSD" : (res.airMode == 0 ? "µ-law" :
                   (res.airMode == 1 ? "A-law" : "transparent")));

            if (rc == 0 && res.connectionHandle != 0) {
                printf("🎧✅ SCO/eSCO 链路建立成功！handle=0x%04X —— 链路层是通的，"
                       "缺的只是 macOS 的音频桥\n", res.connectionHandle);
            } else if (rc != 0 && (unsigned)rc >= 0xE0000000) {
                printf("❌ 控制器/驱动层拒绝（%s）\n", [describeIOReturn(rc) UTF8String]);
            } else {
                printf("❌ 失败，HCI 风格状态 %u (%s)\n", (unsigned)rc, [describeHCIError((uint8_t)rc) UTF8String]);
            }
        }

        printf("\n--- 收尾 ---\n");
        printf("如链路已建立，控制器侧应在数秒内自行释放；本工具不做持久改动。\n");
    }
    return 0;
}
