# 键盘注入实弹验证：开一个置顶 WinForms 观察窗，用 CLI 注入 a/b/1/Enter，
# 把 KeyDown/KeyPress 事件写进日志（证明事件真的经过了系统 HID 栈，而不是只看 CLI 退出码）。
#
# 用法（普通用户即可，不需要管理员）：
#   powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-keyboard-verify.ps1
#   # 可选：-HidCli <vdev-hid-win.exe> -OutDir <日志目录>
param(
    [string]$RepoRoot,
    [string]$HidCli,
    [string]$OutDir
)
$here = $PSScriptRoot
if (-not $RepoRoot) { $RepoRoot = Split-Path -Parent (Split-Path -Parent $here) }
if (-not $OutDir) { $OutDir = Join-Path $env:TEMP 'vdev-acceptance' }
if (-not $HidCli) {
    $HidCli = Join-Path $RepoRoot 'crates\vdev-hid-win\target\x86_64-pc-windows-msvc\release\vdev-hid-win.exe'
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
if (-not (Test-Path $HidCli)) {
    throw "找不到 vdev-hid-win.exe：$HidCli`n先构建：cd crates\vdev-hid-win; cargo build --release"
}

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -Namespace Win -Name Api3 -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
'@ -ReferencedAssemblies System.Drawing, System.Windows.Forms

$exe = $HidCli
$log = Join-Path $OutDir 'kbd-events.txt'
if (Test-Path $log) { [System.IO.File]::Delete($log) }

$form = New-Object System.Windows.Forms.Form
$form.Text = "vdev keyboard observer"
$form.StartPosition = "Manual"
$form.Location = New-Object System.Drawing.Point(500, 320)
$form.Size = New-Object System.Drawing.Size(420, 300)
$form.TopMost = $true
$form.KeyPreview = $true
$form.Add_KeyDown({ param($s, $e) Add-Content -LiteralPath $log ("KeyDown " + $e.KeyCode) })
$form.Add_KeyPress({ param($s, $e) Add-Content -LiteralPath $log ("KeyPress '" + $e.KeyChar + "' (" + [int][char]$e.KeyChar + ")") })

$script:step = 0
# 注入前必须确认观察窗是前台窗口：否则按键会打进"当时前台的那个窗口"（可能是用户的编辑器），
# 而且日志会变成"零事件"的假失败。拿不到焦点就停止计时器并让整个脚本 exit 1。
$script:focusLost = $false
function Confirm-Foreground([System.Windows.Forms.Form]$f) {
    for ($i = 0; $i -lt 10; $i++) {
        if ([Win.Api3]::GetForegroundWindow() -eq $f.Handle) { return $true }
        [void][Win.Api3]::SetForegroundWindow($f.Handle)
        $f.Activate()
        $f.Focus()
        Start-Sleep -Milliseconds 200
    }
    return ([Win.Api3]::GetForegroundWindow() -eq $f.Handle)
}
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 1200
$timer.Add_Tick({
    $script:step++
    switch ($script:step) {
        1 {
            [void][Win.Api3]::SetCursorPos(710, 470)
            if (-not (Confirm-Foreground $form)) {
                Add-Content -LiteralPath $log 'foreground_is_form=False（拿不到焦点，已中止，未注入任何按键）'
                $script:focusLost = $true
                $timer.Stop(); $form.Close(); return
            }
            Add-Content -LiteralPath $log 'foreground_is_form=True'
            & $exe kernel key a 2>&1 | Out-Null
        }
        2 { & $exe kernel key b 2>&1 | Out-Null }
        3 { & $exe kernel key 1 2>&1 | Out-Null }
        4 { & $exe kernel key enter 2>&1 | Out-Null }
        5 { $timer.Stop(); $form.Close() }
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)

Write-Output "键盘观察窗口记录："
Get-Content -LiteralPath $log | ForEach-Object { "    $_" }

# 判定：观察窗必须真的收到 KeyDown/KeyPress。空日志 = 注入没生效（别把"CLI 打印已注入"当通过）。
$keyEvents = @(Get-Content -LiteralPath $log -ErrorAction SilentlyContinue | Where-Object { $_ -match '^Key' })
if ($script:focusLost) {
    Write-Output '❌ 观察窗没拿到前台焦点：已中止注入（避免打进别的窗口）。请关掉抢占焦点的窗口后重跑。'
    exit 1
}
if ($keyEvents.Count -eq 0) {
    Write-Output '❌ 未记录到任何按键事件：注入未生效（或观察窗没拿到前台焦点）'
    exit 1
}
Write-Output ("✅ 记录到 {0} 条按键事件（KeyDown/KeyPress）" -f $keyEvents.Count)
exit 0
