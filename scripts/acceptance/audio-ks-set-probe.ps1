# 对照探测 KSPROPERTY_PIN_PROPOSEDATAFORMAT(14) 的 **SET** 语义：
#   - 同一批候选格式分别发给 vdev 与 ToDesk 的 wave host pin
#   - 打印返回 status / 回填长度 / 回填内容 + vdev 侧 prop_set 计数增量
# 用途：判定"驱动该不该按设备格式做校验""要不要回填缓冲"。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-ks-set-probe.ps1
#   ROOT\MEDIA\<实例号> 随重装变化，默认 vdev=0001 / 对照=0000，可用参数覆盖。
param(
    [string]$VdevInstance = '0001',
    [string]$RefInstance = '0000',
    [string]$RefName = 'ToDesk'
)
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Collections.Generic;

public static class KsSet
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(IntPtr h, uint code, IntPtr inBuf, uint inLen, IntPtr outBuf, uint outLen, out uint ret, IntPtr ov);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr h);

    const uint IOCTL_KS_PROPERTY = 0x002F0003;
    const uint GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000, OPEN_EXISTING = 3;
    const uint GET = 1, SET = 2, BASICSUPPORT = 0x200;
    static readonly Guid KSPSETID_PIN = new Guid("8C134960-51AD-11CF-878A-94F801C10000");
    static readonly Guid KSPROPSETID_VDEV_DEBUG = new Guid("7f4e2a11-9c3b-4b6e-8f2a-1d2c3b4a5e60");

    static IntPtr Open(string path, bool write)
    {
        IntPtr h = CreateFileW(path, write ? (GENERIC_READ | GENERIC_WRITE) : GENERIC_READ, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1) h = CreateFileW(path, GENERIC_READ, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        return h;
    }

    /// 发一次 KSP_PIN 型属性的 SET：实例在输入缓冲，值在输出缓冲（KS 的 METHOD_NEITHER 约定）
    public static string SetPin(string path, uint pin, uint id, byte[] value, int outCap)
    {
        IntPtr h = Open(path, true);
        if (h.ToInt64() == -1) return "OPENFAIL err=" + Marshal.GetLastWin32Error();
        IntPtr inBuf = Marshal.AllocHGlobal(64);
        IntPtr outBuf = Marshal.AllocHGlobal(outCap);
        try
        {
            byte[] g = KSPSETID_PIN.ToByteArray();
            Marshal.Copy(g, 0, inBuf, 16);
            Marshal.WriteInt32(inBuf, 16, (int)id);
            Marshal.WriteInt32(inBuf, 20, (int)SET);
            Marshal.WriteInt32(inBuf, 24, (int)pin);
            Marshal.WriteInt32(inBuf, 28, 0);
            for (int i = 0; i < outCap; i++) Marshal.WriteByte(outBuf, i, 0xCD);
            Marshal.Copy(value, 0, outBuf, value.Length);
            uint ret;
            bool ok = DeviceIoControl(h, IOCTL_KS_PROPERTY, inBuf, 32, outBuf, (uint)outCap, out ret, IntPtr.Zero);
            int err = Marshal.GetLastWin32Error();
            byte[] back = new byte[Math.Min(outCap, 112)];
            Marshal.Copy(outBuf, back, 0, back.Length);
            StringBuilder sb = new StringBuilder();
            sb.Append(ok ? "OK" : "FAIL err=" + err).Append(" ret=").Append(ret).Append(" back=");
            for (int i = 0; i < back.Length; i++) sb.Append(back[i].ToString("x2")).Append(i % 4 == 3 ? " " : "");
            return sb.ToString();
        }
        finally { Marshal.FreeHGlobal(inBuf); Marshal.FreeHGlobal(outBuf); CloseHandle(h); }
    }

    /// vdev 诊断计数器：返回 prop_set / prop_set_ok 两个计数（索引 12 / 32）
    public static string Stats(string path)
    {
        IntPtr h = Open(path, false);
        if (h.ToInt64() == -1) return "OPENFAIL err=" + Marshal.GetLastWin32Error();
        IntPtr inBuf = Marshal.AllocHGlobal(64);
        IntPtr outBuf = Marshal.AllocHGlobal(512);
        try
        {
            byte[] g = KSPROPSETID_VDEV_DEBUG.ToByteArray();
            Marshal.Copy(g, 0, inBuf, 16);
            Marshal.WriteInt32(inBuf, 16, 0);
            Marshal.WriteInt32(inBuf, 20, (int)GET);
            uint ret;
            bool ok = DeviceIoControl(h, IOCTL_KS_PROPERTY, inBuf, 24, outBuf, 512, out ret, IntPtr.Zero);
            if (!ok) return "IOCTLFAIL err=" + Marshal.GetLastWin32Error();
            return "prop_set=" + Marshal.ReadInt32(outBuf, 12 * 4) + " prop_set_ok=" + Marshal.ReadInt32(outBuf, 32 * 4)
                 + " prop_get=" + Marshal.ReadInt32(outBuf, 13 * 4)
                 + " last_tag=0x" + Marshal.ReadInt32(outBuf, 14 * 4).ToString("X8")
                 + " last_rate=" + Marshal.ReadInt32(outBuf, 50 * 4)
                 + " last_bits=" + Marshal.ReadInt32(outBuf, 51 * 4)
                 + " reject=" + Marshal.ReadInt32(outBuf, 52 * 4);
        }
        finally { Marshal.FreeHGlobal(inBuf); Marshal.FreeHGlobal(outBuf); CloseHandle(h); }
    }

    /// 造一个 KSDATAFORMAT_WAVEFORMATEXTENSIBLE(104B) 或裸 WAVEFORMATEX(nB)
    public static byte[] FmtExt(ushort tag, ushort ch, int rate, ushort bits, bool extensible, bool ieeeFloat)
    {
        // WaveFormatExtensible subformat GUID：PCM = 00000001-0000-0010-8000-00AA00389B71
        byte[] sub = ieeeFloat
            ? new Guid("00000003-0000-0010-8000-00AA00389B71").ToByteArray()
            : new Guid("00000001-0000-0010-8000-00AA00389B71").ToByteArray();
        List<byte> b = new List<byte>();
        if (extensible)
        {
            b.AddRange(BitConverter.GetBytes(104));                 // FormatSize
            b.AddRange(BitConverter.GetBytes(0));                   // Flags
            b.AddRange(BitConverter.GetBytes(0));                   // SampleSize
            b.AddRange(BitConverter.GetBytes(0));                   // Reserved
            b.AddRange(new Guid("73647561-0000-0010-8000-00AA00389B71").ToByteArray()); // TYPE_AUDIO
            b.AddRange(sub);                                        // SubFormat
            b.AddRange(new Guid("05589f81-c6ce-11bf-ab01-00aa0055595a").ToByteArray()); // SPECIFIER_WAVEFORMATEX
        }
        b.AddRange(BitConverter.GetBytes(tag));
        b.AddRange(BitConverter.GetBytes(ch));
        b.AddRange(BitConverter.GetBytes(rate));
        b.AddRange(BitConverter.GetBytes(rate * ch * bits / 8));
        b.AddRange(BitConverter.GetBytes((ushort)(ch * bits / 8)));
        b.AddRange(BitConverter.GetBytes(bits));
        b.AddRange(BitConverter.GetBytes(extensible ? (ushort)22 : (ushort)0));
        if (extensible)
        {
            b.AddRange(BitConverter.GetBytes(bits));                 // valid bits
            b.AddRange(BitConverter.GetBytes(ch == 2 ? 3 : 4));      // channel mask
            b.AddRange(sub);
        }
        return b.ToArray();
    }
}
'@

$cat = '{6994ad04-93ef-11d0-a3cc-00a0c9223196}'
$devices = @(
    @{ Name = 'vdev   WaveRender-0'; Path = "\\?\ROOT#MEDIA#$VdevInstance#$cat\WaveRender-0" },
    @{ Name = "$RefName WaveRender-0"; Path = "\\?\ROOT#MEDIA#$RefInstance#$cat\WaveRender-0" }
)

$cases = @(
    @{ Desc = 'EXTENSIBLE PCM 16bit 48000 2ch'; Fmt = [KsSet]::FmtExt(0xFFFE, 2, 48000, 16, $true, $false) },
    @{ Desc = 'EXTENSIBLE PCM 16bit 44100 2ch'; Fmt = [KsSet]::FmtExt(0xFFFE, 2, 44100, 16, $true, $false) },
    @{ Desc = 'EXTENSIBLE float32 48000 2ch';   Fmt = [KsSet]::FmtExt(0xFFFE, 2, 48000, 32, $true, $true) },
    @{ Desc = 'bare WAVEFORMATEX PCM 16/48000'; Fmt = [KsSet]::FmtExt(1, 2, 48000, 16, $false, $false) }
)

foreach ($d in $devices) {
    Write-Output ("===== {0}" -f $d.Name)
    if ($d.Name -match 'vdev') { Write-Output ("  before: {0}" -f [KsSet]::Stats($d.Path)) }
    foreach ($c in $cases) {
        $r = [KsSet]::SetPin($d.Path, 0, 14, $c.Fmt, 256)
        Write-Output ("  SET pin0 id14 [{0}] len={1} -> {2}" -f $c.Desc, $c.Fmt.Length, $r)
    }
    if ($d.Name -match 'vdev') { Write-Output ("  after : {0}" -f [KsSet]::Stats($d.Path)) }
}
