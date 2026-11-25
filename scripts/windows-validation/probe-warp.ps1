param([Parameter(Mandatory)][string]$OutputDirectory)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$evidence = (New-Item -Force -ItemType Directory -Path $OutputDirectory).FullName
$log = Join-Path $evidence 'warp-probe.log'
$source = Join-Path $PSScriptRoot 'warp-probe.cpp'
$build = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString()))
$probe = Join-Path $build.FullName 'warp-probe.exe'

function Invoke-WarpProbe {
    & $probe 2>&1 | Tee-Object -FilePath $log -Append | Out-Host
    return $LASTEXITCODE
}

Push-Location $build.FullName
try {
    $locator = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $vs = & $locator -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vs) { throw 'Visual Studio x64 C++ build tools are required.' }
    Import-Module (Join-Path $vs 'Common7/Tools/Microsoft.VisualStudio.DevShell.dll')
    Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64'
    & cl.exe /nologo /EHsc /std:c++17 $source /Fewarp-probe.exe /link d3d12.lib dxgi.lib 2>&1 |
        Tee-Object -FilePath $log
    if ($LASTEXITCODE -ne 0) { throw "WARP probe compilation failed. See $log" }
    $result = Invoke-WarpProbe
    if ($result -in 1, 2) {
        $capability = 'Tools.Graphics.DirectX~~~~0.0.1.0'
        $retryProbe = $false
        try {
            $state = (Get-WindowsCapability -Online -Name $capability -ErrorAction Stop).State
            "Graphics Tools state: $state" | Tee-Object -FilePath $log -Append
            if ($state -eq 'NotPresent') {
                $installed = Add-WindowsCapability -Online -Name $capability -ErrorAction Stop
                "Graphics Tools installation returned. RestartNeeded=$($installed.RestartNeeded)" |
                    Tee-Object -FilePath $log -Append
                $retryProbe = $true
            }
        } catch {
            "Graphics Tools query or installation failed: $_" | Tee-Object -FilePath $log -Append
        }
        if ($retryProbe) {
            'Probing after the Graphics Tools installation attempt.' | Tee-Object -FilePath $log -Append
            $result = Invoke-WarpProbe
        }
    }
    if ($result -eq 2) {
        Write-Warning 'WARP supports the required RG8 operations. The debug interface is unavailable.'
    } elseif ($result -ne 0) {
        throw "WARP software device or RG8 capability check failed ($result). See $log"
    }
} finally {
    Pop-Location
    Remove-Item -LiteralPath $build.FullName -Recurse -Force -ErrorAction SilentlyContinue
}
exit 0
