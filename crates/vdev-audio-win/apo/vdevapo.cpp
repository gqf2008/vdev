// vdevapo.cpp — 最小 SFX/MFX APO（可行性探针）
// 目的：验证 Windows 音频引擎能否加载本 DLL 并在渲染链路上调用 APOProcess。
// 行为：直通 + 固定 ×0.5 增益（便于用环回 RMS 量化是否真的生效），并写日志到
//       C:\Windows\Temp\vdevapo.log。
// 只实现必需接口：IAudioProcessingObject / IAudioProcessingObjectConfiguration /
// IAudioProcessingObjectRT，外加 IAudioSystemEffects2 供引擎枚举。
// 接口签名取自公开 Windows SDK 头 audioenginebaseapo.h（随 WDK/SDK 分发的公开头文件）。
// 无第三方依赖、无第三方数据文件。

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <mmreg.h>
#include <objbase.h>
#include <new>
#include <stdio.h>
#include <stdarg.h>

typedef LONGLONG HNSTIME;              // 通常由 ks.h 提供；此处自带以避免 ks.h 的 GUID_NULL 宏污染
#include <audioenginebaseapo.h>

static const CLSID CLSID_VdevApo =
    {0x8b7a9c3e, 0x5d2f, 0x4a11, {0x9c, 0x6e, 0x2f, 0x1b, 0x7d, 0x4e, 0x0a, 0x55}};

#define APO_LOG "C:\\Windows\\Temp\\vdevapo.log"

static void ApoLog(const char *fmt, ...)
{
    FILE *f = NULL;
    if (fopen_s(&f, APO_LOG, "a") != 0 || !f) return;
    SYSTEMTIME st; GetLocalTime(&st);
    fprintf(f, "[%02d:%02d:%02d.%03d pid=%lu] ", st.wHour, st.wMinute, st.wSecond,
            st.wMilliseconds, (unsigned long)GetCurrentProcessId());
    va_list ap; va_start(ap, fmt); vfprintf(f, fmt, ap); va_end(ap);
    fprintf(f, "\n");
    fclose(f);
}

static LONG g_cLock = 0;
static LONG g_instances = 0;

class CVdevApo : public IAudioProcessingObject,
                 public IAudioProcessingObjectConfiguration,
                 public IAudioProcessingObjectRT,
                 public IAudioSystemEffects2
{
public:
    CVdevApo() : m_cRef(1), m_channels(2), m_frames(0)
    {
        InterlockedIncrement(&g_instances);
        ApoLog("CVdevApo::ctor instances=%ld", (long)g_instances);
    }
    ~CVdevApo()
    {
        InterlockedDecrement(&g_instances);
        ApoLog("CVdevApo::dtor instances=%ld", (long)g_instances);
    }

    // ---- IUnknown ----
    STDMETHOD(QueryInterface)(REFIID riid, void **ppv)
    {
        if (!ppv) return E_POINTER;
        *ppv = NULL;
        if (riid == __uuidof(IUnknown) || riid == __uuidof(IAudioProcessingObject))
            *ppv = static_cast<IAudioProcessingObject *>(this);
        else if (riid == __uuidof(IAudioProcessingObjectConfiguration))
            *ppv = static_cast<IAudioProcessingObjectConfiguration *>(this);
        else if (riid == __uuidof(IAudioProcessingObjectRT))
            *ppv = static_cast<IAudioProcessingObjectRT *>(this);
        else if (riid == __uuidof(IAudioSystemEffects2))
            *ppv = static_cast<IAudioSystemEffects2 *>(this);
        else if (riid == __uuidof(IAudioSystemEffects))
            *ppv = static_cast<IAudioSystemEffects *>(this);
        else
        {
            ApoLog("QI miss");
            return E_NOINTERFACE;
        }
        AddRef();
        return S_OK;
    }
    STDMETHOD_(ULONG, AddRef)() { return (ULONG)InterlockedIncrement(&m_cRef); }
    STDMETHOD_(ULONG, Release)()
    {
        LONG n = InterlockedDecrement(&m_cRef);
        if (n == 0) delete this;
        return (ULONG)n;
    }

    // ---- IAudioProcessingObject ----
    STDMETHOD(Reset)() { ApoLog("Reset"); return S_OK; }
    STDMETHOD(GetLatency)(HNSTIME *pTime) { if (pTime) *pTime = 0; return S_OK; }
    STDMETHOD(GetRegistrationProperties)(APO_REG_PROPERTIES **ppRegProps)
    {
        ApoLog("GetRegistrationProperties");
        if (!ppRegProps) return E_POINTER;
        APO_REG_PROPERTIES *p = (APO_REG_PROPERTIES *)CoTaskMemAlloc(sizeof(APO_REG_PROPERTIES));
        if (!p) return E_OUTOFMEMORY;
        ZeroMemory(p, sizeof(*p));
        p->clsid = CLSID_VdevApo;
        p->Flags = APO_FLAG_DEFAULT;
        lstrcpynW(p->szFriendlyName, L"vdev APO probe", 256);
        lstrcpynW(p->szCopyrightInfo, L"probe", 256);
        p->u32MajorVersion = 1;
        p->u32MinorVersion = 0;
        p->u32MinInputConnections = 1;
        p->u32MaxInputConnections = 1;
        p->u32MinOutputConnections = 1;
        p->u32MaxOutputConnections = 1;
        p->u32MaxInstances = 0xFFFFFFFF;
        p->u32NumAPOInterfaces = 1;
        p->iidAPOInterfaceList[0] = __uuidof(IAudioProcessingObject);
        *ppRegProps = p;
        return S_OK;
    }
    STDMETHOD(Initialize)(UINT32 cbDataSize, BYTE *pbyData)
    {
        ApoLog("Initialize cbDataSize=%lu", (unsigned long)cbDataSize);
        return S_OK;
    }
    STDMETHOD(IsInputFormatSupported)(IAudioMediaType *pOppositeFormat,
                                      IAudioMediaType *pRequestedInputFormat,
                                      IAudioMediaType **ppSupportedInputFormat)
    {
        ApoLog("IsInputFormatSupported");
        if (!ppSupportedInputFormat) return E_POINTER;
        *ppSupportedInputFormat = NULL;
        if (pRequestedInputFormat)
        { *ppSupportedInputFormat = pRequestedInputFormat;
          pRequestedInputFormat->AddRef(); return S_OK; }
        return S_FALSE;
    }
    STDMETHOD(IsOutputFormatSupported)(IAudioMediaType *pOppositeFormat,
                                       IAudioMediaType *pRequestedOutputFormat,
                                       IAudioMediaType **ppSupportedOutputFormat)
    {
        ApoLog("IsOutputFormatSupported");
        if (!ppSupportedOutputFormat) return E_POINTER;
        *ppSupportedOutputFormat = NULL;
        if (pRequestedOutputFormat)
        { *ppSupportedOutputFormat = pRequestedOutputFormat;
          pRequestedOutputFormat->AddRef(); return S_OK; }
        return S_FALSE;
    }
    STDMETHOD(GetInputChannelCount)(UINT32 *pu32ChannelCount)
    { if (pu32ChannelCount) *pu32ChannelCount = m_channels; return S_OK; }

    // ---- IAudioSystemEffects2 ----
    STDMETHOD(GetEffectsList)(LPGUID *ppEffectsIds, UINT *pcEffects, HANDLE Event)
    {
        ApoLog("GetEffectsList");
        if (ppEffectsIds) *ppEffectsIds = NULL;
        if (pcEffects) *pcEffects = 0;
        return S_OK;
    }

    // ---- IAudioProcessingObjectConfiguration ----
    STDMETHOD(LockForProcess)(UINT32 u32NumInputConnections,
                              APO_CONNECTION_DESCRIPTOR **ppInputConnections,
                              UINT32 u32NumOutputConnections,
                              APO_CONNECTION_DESCRIPTOR **ppOutputConnections)
    {
        m_frames = 0; m_channels = 2;
        if (u32NumInputConnections >= 1 && ppInputConnections && ppInputConnections[0])
        {
            APO_CONNECTION_DESCRIPTOR *d = ppInputConnections[0];
            m_frames = d->u32MaxFrameCount;
            if (d->pFormat)
            {
                UNCOMPRESSEDAUDIOFORMAT uf; ZeroMemory(&uf, sizeof(uf));
                if (SUCCEEDED(d->pFormat->GetUncompressedAudioFormat(&uf)) && uf.dwSamplesPerFrame)
                    m_channels = uf.dwSamplesPerFrame;
            }
        }
        ApoLog("LockForProcess in=%lu out=%lu frames=%lu ch=%lu",
               (unsigned long)u32NumInputConnections, (unsigned long)u32NumOutputConnections,
               (unsigned long)m_frames, (unsigned long)m_channels);
        return S_OK;
    }
    STDMETHOD(UnlockForProcess)() { ApoLog("UnlockForProcess"); return S_OK; }

    // ---- IAudioProcessingObjectRT ----
    void STDMETHODCALLTYPE APOProcess(UINT32 u32NumInputConnections,
                                      APO_CONNECTION_PROPERTY **ppInputConnections,
                                      UINT32 u32NumOutputConnections,
                                      APO_CONNECTION_PROPERTY **ppOutputConnections)
    {
        static LONG calls = 0;
        LONG c = InterlockedIncrement(&calls);
        if (c <= 3 || (c % 5000) == 0)
            ApoLog("APOProcess call#%ld in=%lu out=%lu", c,
                   (unsigned long)u32NumInputConnections, (unsigned long)u32NumOutputConnections);

        UINT32 n = u32NumInputConnections < u32NumOutputConnections
                       ? u32NumInputConnections : u32NumOutputConnections;
        for (UINT32 i = 0; i < n; i++)
        {
            APO_CONNECTION_PROPERTY *in = ppInputConnections ? ppInputConnections[i] : NULL;
            APO_CONNECTION_PROPERTY *out = ppOutputConnections ? ppOutputConnections[i] : NULL;
            if (!in || !out || !in->pBuffer || !out->pBuffer) continue;
            UINT32 samples = in->u32ValidFrameCount * m_channels;
            float *s = (float *)in->pBuffer;
            float *d = (float *)out->pBuffer;
            for (UINT32 k = 0; k < samples; k++) d[k] = s[k] * 0.5f;   // ×0.5 增益（可测）
            out->u32ValidFrameCount = in->u32ValidFrameCount;
            out->u32BufferFlags = in->u32BufferFlags;
        }
    }
    UINT32 STDMETHODCALLTYPE CalcInputFrames(UINT32 u32OutputFrameCount)
    { return u32OutputFrameCount; }
    UINT32 STDMETHODCALLTYPE CalcOutputFrames(UINT32 u32InputFrameCount)
    { return u32InputFrameCount; }

private:
    LONG m_cRef;
    UINT32 m_channels;
    UINT32 m_frames;
};

class CClassFactory : public IClassFactory
{
public:
    CClassFactory() : m_cRef(1) {}
    STDMETHOD(QueryInterface)(REFIID riid, void **ppv)
    {
        if (!ppv) return E_POINTER;
        *ppv = NULL;
        if (riid == __uuidof(IUnknown) || riid == __uuidof(IClassFactory))
        { *ppv = static_cast<IClassFactory *>(this); AddRef(); return S_OK; }
        return E_NOINTERFACE;
    }
    STDMETHOD_(ULONG, AddRef)() { return (ULONG)InterlockedIncrement(&m_cRef); }
    STDMETHOD_(ULONG, Release)()
    { LONG n = InterlockedDecrement(&m_cRef); if (n == 0) delete this; return (ULONG)n; }
    STDMETHOD(CreateInstance)(IUnknown *pUnkOuter, REFIID riid, void **ppv)
    {
        ApoLog("ClassFactory::CreateInstance");
        if (pUnkOuter) return CLASS_E_NOAGGREGATION;
        CVdevApo *p = new (std::nothrow) CVdevApo();
        if (!p) return E_OUTOFMEMORY;
        HRESULT hr = p->QueryInterface(riid, ppv);
        p->Release();
        return hr;
    }
    STDMETHOD(LockServer)(BOOL b)
    { b ? InterlockedIncrement(&g_cLock) : InterlockedDecrement(&g_cLock); return S_OK; }
private:
    LONG m_cRef;
};

BOOL APIENTRY DllMain(HMODULE, DWORD reason, LPVOID)
{
    if (reason == DLL_PROCESS_ATTACH) ApoLog("DllMain DLL_PROCESS_ATTACH");
    return TRUE;
}

STDAPI DllGetClassObject(REFCLSID rclsid, REFIID riid, void **ppv)
{
    ApoLog("DllGetClassObject");
    if (!ppv) return E_POINTER;
    *ppv = NULL;
    if (rclsid != CLSID_VdevApo) return CLASS_E_CLASSNOTAVAILABLE;
    CClassFactory *f = new (std::nothrow) CClassFactory();
    if (!f) return E_OUTOFMEMORY;
    HRESULT hr = f->QueryInterface(riid, ppv);
    f->Release();
    return hr;
}

STDAPI DllCanUnloadNow()
{
    return (g_cLock == 0 && g_instances == 0) ? S_OK : S_FALSE;
}

static void ClsidToStr(const CLSID &c, wchar_t *out)
{
    wsprintfW(out, L"{%08lX-%04hX-%04hX-%02X%02X-%02X%02X%02X%02X%02X%02X}",
              c.Data1, c.Data2, c.Data3,
              c.Data4[0], c.Data4[1], c.Data4[2], c.Data4[3],
              c.Data4[4], c.Data4[5], c.Data4[6], c.Data4[7]);
}

// 注册到 HKCR\CLSID\{GUID}\InprocServer32（HKCR\CLSID 才是 COM 实际解析的位置；
// 早期版本误写成 HKCR\{GUID}，那条路径 COM 不会查，导致 regsvr32 注册后仍找不到组件）。
STDAPI DllRegisterServer()
{
    ApoLog("DllRegisterServer");
    wchar_t path[MAX_PATH]; HMODULE h = NULL;
    GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                       (LPCWSTR)&DllRegisterServer, &h);
    GetModuleFileNameW(h, path, MAX_PATH);
    wchar_t sub[512];
    lstrcpynW(sub, L"CLSID\\", 512);
    ClsidToStr(CLSID_VdevApo, sub + lstrlenW(sub));
    HKEY k, k2; DWORD disp;
    if (RegCreateKeyExW(HKEY_CLASSES_ROOT, sub, 0, NULL, 0, KEY_WRITE, NULL, &k, &disp) != ERROR_SUCCESS)
        return E_FAIL;
    RegSetValueExW(k, L"", 0, REG_SZ, (const BYTE *)L"vdev APO probe",
                   (DWORD)((lstrlenW(L"vdev APO probe") + 1) * sizeof(wchar_t)));
    if (RegCreateKeyExW(k, L"InprocServer32", 0, NULL, 0, KEY_WRITE, NULL, &k2, &disp) != ERROR_SUCCESS)
    { RegCloseKey(k); return E_FAIL; }
    RegSetValueExW(k2, L"", 0, REG_SZ, (const BYTE *)path,
                   (DWORD)((lstrlenW(path) + 1) * sizeof(wchar_t)));
    RegSetValueExW(k2, L"ThreadingModel", 0, REG_SZ, (const BYTE *)L"Both",
                   (DWORD)((lstrlenW(L"Both") + 1) * sizeof(wchar_t)));
    RegCloseKey(k2); RegCloseKey(k);
    return S_OK;
}

STDAPI DllUnregisterServer()
{
    ApoLog("DllUnregisterServer");
    wchar_t sub[512];
    lstrcpynW(sub, L"CLSID\\", 512);
    ClsidToStr(CLSID_VdevApo, sub + lstrlenW(sub));
    RegDeleteKeyW(HKEY_CLASSES_ROOT, sub);
    return S_OK;
}
