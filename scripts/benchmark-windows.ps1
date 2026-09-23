param(
    [string] $Executable = (Join-Path $PSScriptRoot '..\target\release\utterly.exe'),
    [int] $IdleSeconds = 30,
    [int] $DragSeconds = 6
)

$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path $Executable).Path
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$stage = Join-Path $env:TEMP ("utterly-benchmark-package-" + [guid]::NewGuid().ToString('N'))
$benchmarkProfile = Join-Path $env:TEMP ("utterly-benchmark-profile-" + [guid]::NewGuid().ToString('N'))
$profileApp = Join-Path $benchmarkProfile 'Utterly'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$null = New-Item -ItemType Directory -Force -Path (Join-Path $stage 'assets'), $profileApp

Copy-Item $exe (Join-Path $stage 'utterly-windows-x86_64.exe')
Copy-Item (Join-Path $repo 'assets\utterly.ico') (Join-Path $stage 'assets\utterly.ico')
$zip = "$stage.zip"
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $stage,
    $zip,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $false
)

# A dummy nonempty key keeps first-run onboarding from opening a browser.
# It is isolated to this temporary profile and never used for a connection.
[ordered]@{
    mic = ''
    hotkey = 'Ctrl+Space'
    api_key = 'benchmark-placeholder'
    language_codes = @()
    mode = 'smart'
    custom_vocabulary = @()
} | ConvertTo-Json -Depth 4 | ForEach-Object {
    [System.IO.File]::WriteAllText(
        (Join-Path $profileApp 'config.json'),
        $_,
        [System.Text.UTF8Encoding]::new($false)
    )
}

if (-not ('UtterlyBenchmarkWin32' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public sealed class UtterlyBenchmarkWindow
{
    public IntPtr Handle;
    public string Title;
    public int Left, Top, Right, Bottom;
    public int Width { get { return Right - Left; } }
    public int Height { get { return Bottom - Top; } }
}

public static class UtterlyBenchmarkWin32
{
    public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr lParam);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int max);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint message, UIntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern uint GetDpiForSystem();

    public static UtterlyBenchmarkWindow[] WindowsForProcess(uint wanted)
    {
        var found = new List<UtterlyBenchmarkWindow>();
        EnumWindows((hwnd, _) => {
            uint processId;
            GetWindowThreadProcessId(hwnd, out processId);
            if (processId == wanted) {
                var title = new StringBuilder(512);
                GetWindowTextW(hwnd, title, title.Capacity);
                RECT r;
                if (GetWindowRect(hwnd, out r)) found.Add(new UtterlyBenchmarkWindow {
                    Handle = hwnd, Title = title.ToString(), Left = r.Left, Top = r.Top,
                    Right = r.Right, Bottom = r.Bottom
                });
            }
            return true;
        }, IntPtr.Zero);
        return found.ToArray();
    }
}
'@
}

function Get-AppWindows([System.Diagnostics.Process] $Process) {
    $Process.Refresh()
    [UtterlyBenchmarkWin32]::WindowsForProcess([uint32] $Process.Id)
}

function Measure-Process([System.Diagnostics.Process] $Process, [int] $Seconds) {
    $Process.Refresh()
    $cpuBefore = $Process.TotalProcessorTime.TotalSeconds
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $maxPrivate = 0L
    $maxWorking = 0L
    while ($clock.Elapsed.TotalSeconds -lt $Seconds) {
        $Process.Refresh()
        $maxPrivate = [Math]::Max($maxPrivate, $Process.PrivateMemorySize64)
        $maxWorking = [Math]::Max($maxWorking, $Process.WorkingSet64)
        Start-Sleep -Milliseconds 200
    }
    $clock.Stop()
    $Process.Refresh()
    $cpuPercent = (($Process.TotalProcessorTime.TotalSeconds - $cpuBefore) / $clock.Elapsed.TotalSeconds) * 100
    [pscustomobject]@{
        Seconds = [Math]::Round($clock.Elapsed.TotalSeconds, 2)
        CpuPercentOneCore = [Math]::Round($cpuPercent, 2)
        PeakPrivateMiB = [Math]::Round($maxPrivate / 1MB, 2)
        PeakWorkingSetMiB = [Math]::Round($maxWorking / 1MB, 2)
    }
}

function Drag-Window([System.Diagnostics.Process] $Process, [UtterlyBenchmarkWindow] $Window, [int] $Seconds, [int] $Dx, [int] $Dy, [bool] $Caption) {
    $before = $Window
    $startX = $Window.Left + [int]($Window.Width / 2)
    $startY = if ($Caption) { $Window.Top + 12 } else { $Window.Top + [int]($Window.Height / 2) }
    $steps = [Math]::Max(30, $Seconds * 30)
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $Process.Refresh()
    $cpuBefore = $Process.TotalProcessorTime.TotalSeconds

    if (-not [UtterlyBenchmarkWin32]::SetCursorPos($startX, $startY)) { throw 'Could not position the pointer for the drag sample.' }
    [UtterlyBenchmarkWin32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    for ($i = 1; $i -le $steps; $i++) {
        $fraction = $i / $steps
        [UtterlyBenchmarkWin32]::SetCursorPos(($startX + [int]($Dx * $fraction)), ($startY + [int]($Dy * $fraction))) | Out-Null
        $targetMs = ($Seconds * 1000 * $i) / $steps
        while ($clock.Elapsed.TotalMilliseconds -lt $targetMs) { Start-Sleep -Milliseconds 1 }
    }
    [UtterlyBenchmarkWin32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    $clock.Stop()
    Start-Sleep -Milliseconds 200
    $after = Get-AppWindows $Process | Where-Object Handle -eq $Window.Handle | Select-Object -First 1
    $Process.Refresh()
    [pscustomobject]@{
        Seconds = [Math]::Round($clock.Elapsed.TotalSeconds, 2)
        CpuPercentOneCore = [Math]::Round((($Process.TotalProcessorTime.TotalSeconds - $cpuBefore) / $clock.Elapsed.TotalSeconds) * 100, 2)
        DeltaX = if ($after) { $after.Left - $before.Left } else { 0 }
        DeltaY = if ($after) { $after.Top - $before.Top } else { 0 }
    }
    # Restore the window where it started so the benchmark leaves the desktop tidy.
    if ($after) {
        [UtterlyBenchmarkWin32]::SetWindowPos($Window.Handle, [IntPtr]::Zero, $before.Left, $before.Top, 0, 0, 0x0001 -bor 0x0004 -bor 0x0010) | Out-Null
    }
}

$previousAppData = $env:APPDATA
$process = $null
try {
    $env:APPDATA = $benchmarkProfile
    $process = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -WindowStyle Hidden -PassThru
    $deadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 200
        $windows = Get-AppWindows $process
        $settings = $windows | Where-Object Title -eq 'Utterly Settings' | Select-Object -First 1
        $pill = $windows | Where-Object { $_.Title.StartsWith('Utterly') -and $_.Title -ne 'Utterly Settings' -and $_.Width -ge 200 -and $_.Height -ge 40 } | Select-Object -First 1
    } until (($settings -and $pill) -or (Get-Date) -gt $deadline)
    if (-not $pill) { throw 'The pill window did not appear within 15 seconds.' }
    Start-Sleep -Seconds 2
    $pillBounds = "$($pill.Width)x$($pill.Height)"
    $settingsBounds = if ($settings) { "$($settings.Width)x$($settings.Height)" } else { 'unavailable' }

    $idle = Measure-Process $process $IdleSeconds
    if ($settings) {
        [UtterlyBenchmarkWin32]::PostMessageW($settings.Handle, 0x8001, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        Start-Sleep -Milliseconds 200
        $settingsOpen = Measure-Process $process ([Math]::Min(5, $IdleSeconds))
        [UtterlyBenchmarkWin32]::PostMessageW($settings.Handle, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        Start-Sleep -Milliseconds 150
        $settingsCloseHides = -not [UtterlyBenchmarkWin32]::IsWindowVisible($settings.Handle)
    }

    $windows = Get-AppWindows $process
    $pill = $windows | Where-Object { $_.Title.StartsWith('Utterly') -and $_.Title -ne 'Utterly Settings' -and $_.Width -ge 200 -and $_.Height -ge 40 } | Select-Object -First 1
    $pillDrag = Drag-Window $process $pill $DragSeconds 72 -36 $false
    if ($settings) {
        [UtterlyBenchmarkWin32]::PostMessageW($settings.Handle, 0x8001, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        Start-Sleep -Milliseconds 200
        $windows = Get-AppWindows $process
        $settings = $windows | Where-Object Title -eq 'Utterly Settings' | Select-Object -First 1
        $settingsDrag = Drag-Window $process $settings $DragSeconds 72 36 $true
    }

    $packageBytes = (Get-Item $zip).Length
    $exeBytes = (Get-Item $exe).Length
    $iconBytes = (Get-Item (Join-Path $repo 'assets\utterly.ico')).Length
    [pscustomobject]@{
        WindowsVersion = [Environment]::OSVersion.Version.ToString()
        Dpi = [UtterlyBenchmarkWin32]::GetDpiForSystem()
        PillBounds = $pillBounds
        SettingsBounds = $settingsBounds
        ExecutableBytes = $exeBytes
        IconBytes = $iconBytes
        ReleaseZipBytes = $packageBytes
        Idle = $idle
        SettingsOpen = $settingsOpen
        PillDrag = $pillDrag
        SettingsCaptionDrag = $settingsDrag
        SettingsCloseHides = $settingsCloseHides
        LiveApiCalled = $false
    } | ConvertTo-Json -Depth 5
}
finally {
    if ($process) {
        $process.Refresh()
        if (-not $process.HasExited -and $process.Path -eq $exe) { Stop-Process -Id $process.Id -Force }
    }
    $env:APPDATA = $previousAppData
}
