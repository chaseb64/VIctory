param(
    [Parameter(Mandatory)][ValidateSet('Checks', 'R4G4')][string]$Suite,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Baseline = '5c16c766033e4052f09a446372c47238d8193cf5',
    [ValidateSet('WARP', 'Physical')][string]$Adapter = 'WARP',
    [switch]$InstallGraphicsTools
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_TERM_COLOR = 'never'
$null = New-Item -ItemType Directory -Force $OutputDirectory
$OutputDirectory = (Resolve-Path $OutputDirectory).Path
$root = (Resolve-Path "$PSScriptRoot/../..").Path
Set-Location $root

function Invoke-Logged([string]$Name, [scriptblock]$Command) {
    $log = Join-Path $OutputDirectory "$Name.log"
    $global:LASTEXITCODE = 0
    & $Command 2>&1 | Tee-Object -FilePath $log | Out-Host
    return $LASTEXITCODE
}

function Assert-Passed([string]$Test, [int]$Code) {
    if ($Code -ne 0) { throw "$Test exited with code $Code" }
}

function Assert-OneTest([string]$Name, [string]$Pattern) {
    $log = Get-Content -Raw (Join-Path $OutputDirectory "$Name.log")
    if ($log -notmatch $Pattern -or $log -notmatch 'test result: ok\. 1 passed; 0 failed; 0 ignored;') {
        throw "$Name did not execute exactly one passing test with the required evidence"
    }
}

function Get-DebugStatus {
    $status = [VictoryValidation.DebugLayer]::Probe()
    'D3D12GetDebugInterface HRESULT=0x{0:X8}' -f $status | Tee-Object -Append (Join-Path $OutputDirectory 'debug-layer.log') | Out-Host
    return $status
}

try {
    Assert-Passed 'Environment' (Invoke-Logged 'environment' {
        "Runner image: $env:ImageOS $env:ImageVersion"
        git rev-parse HEAD
        rustc -Vv
        cargo -V
        Get-CimInstance Win32_OperatingSystem | Format-List Caption, Version, BuildNumber
        Get-CimInstance Win32_VideoController | Format-List Name, DriverVersion
        Get-Item "$env:WINDIR/System32/d3d12.dll","$env:WINDIR/System32/d3d10warp.dll","$env:WINDIR/System32/d3d12sdklayers.dll" -ErrorAction SilentlyContinue |
            Select-Object Name, @{n='Version';e={$_.VersionInfo.FileVersion}} | Format-Table
    })
    if ($Suite -eq 'Checks') {
        $results = [ordered]@{}
        $results['r4g4-cpu'] = Invoke-Logged 'r4g4-cpu' { cargo test --locked -j 2 -p victory-gpu --features diagnostics r4g4 -- --test-threads=1 }
        $results['workspace-tests'] = Invoke-Logged 'workspace-tests' { cargo test --workspace --locked -j 2 }
        $results['clippy'] = Invoke-Logged 'clippy' { cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings }
        $results['clippy-diagnostics'] = Invoke-Logged 'clippy-diagnostics' { cargo clippy --workspace --all-targets --all-features --locked -j 2 -- -D warnings }
        $results['asset-policy'] = Invoke-Logged 'asset-policy' { cargo xtask policy }
        $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'status.json')
        $results.GetEnumerator() | ForEach-Object { Assert-Passed $_.Key $_.Value }
        exit 0
    }

    $env:VICTORY_D3D12_DEBUG = '1'
    $env:VICTORY_REQUIRE_D3D12_DEBUG = '1'
    if ($Adapter -eq 'WARP') { $env:VICTORY_D3D12_WARP = '1' }
    else { Remove-Item Env:VICTORY_D3D12_WARP -ErrorAction SilentlyContinue }

    Add-Type @'
using System;
using System.Runtime.InteropServices;

namespace VictoryValidation {
    public static class DebugLayer {
        [DllImport("d3d12.dll", ExactSpelling = true)]
        static extern int D3D12GetDebugInterface(ref Guid iid, out IntPtr instance);
        public static int Probe() {
            var iid = new Guid("344488b7-6846-474b-b989-f027448245e0");
            IntPtr instance;
            int hr = D3D12GetDebugInterface(ref iid, out instance);
            if (instance != IntPtr.Zero) Marshal.Release(instance);
            return hr;
        }
    }
}
'@
    $status = Get-DebugStatus
    if ($status -lt 0 -and $InstallGraphicsTools) {
        try {
            $capability = 'Tools.Graphics.DirectX~~~~0.0.1.0'
            Get-WindowsCapability -Online -Name $capability |
                Format-List | Out-File (Join-Path $OutputDirectory 'graphics-tools.log')
            Add-WindowsCapability -Online -Name $capability |
                Format-List | Out-File -Append (Join-Path $OutputDirectory 'graphics-tools.log')
        } catch {
            $_ | Out-File -Append (Join-Path $OutputDirectory 'graphics-tools.log')
        }
        $status = Get-DebugStatus
    }

    $gpu = 'resource::tests::r4g4_gpu_test::sampled_r4g4_gpu_upload_copy_and_sample'
    $oracle = 'resource::windows_validation_r4g4::shared_r4g4_host_boundary'
    $overlay = Join-Path $PSScriptRoot 'r4g4_host_boundary.rs'
    $startupPatch = Join-Path $PSScriptRoot 'fmad-probe.patch.txt'
    Get-FileHash $startupPatch | Format-List | Out-File (Join-Path $OutputDirectory 'startup-patch.sha256.txt')
    $revisions = @(@{ Name='candidate'; Ref='HEAD' })
    if ($Adapter -eq 'WARP') { $revisions += @{ Name='parent'; Ref=$Baseline } }

    foreach ($revision in $revisions) {
        $tree = Join-Path ([System.IO.Path]::GetTempPath()) ("victory-" + [guid]::NewGuid())
        git worktree add --detach $tree $revision.Ref
        Assert-Passed 'Create temporary worktree' $LASTEXITCODE
        Push-Location $tree
        try {
            $env:CARGO_TARGET_DIR = Join-Path $root "target/validation-$($revision.Name)"
            $name = "$($revision.Name)-host-boundary"
            git rev-parse HEAD | Set-Content (Join-Path $OutputDirectory "$name.revision.txt")
            Assert-Passed 'Apply shared startup probe correction' (Invoke-Logged "$($revision.Name)-startup-patch" {
                git apply --check --verbose $startupPatch
                if ($LASTEXITCODE -eq 0) { git apply --verbose $startupPatch }
                if ($LASTEXITCODE -eq 0) { git diff --stat }
            })

            if ($revision.Name -eq 'candidate') {
                $gpuCode = Invoke-Logged 'candidate-d3d12' {
                    cargo test --locked -j 2 -p victory-gpu --features diagnostics $gpu -- --exact --ignored --nocapture --test-threads=1
                }
            }
            if ($Adapter -eq 'WARP') {
                Add-Content 'crates/victory-gpu/src/resource.rs' ("`n" + (Get-Content -Raw $overlay))
                $code = Invoke-Logged $name {
                    cargo test --locked -j 2 -p victory-gpu --features diagnostics $oracle -- --exact --ignored --nocapture --test-threads=1
                }
                if ($revision.Name -eq 'parent') {
                    $log = Get-Content -Raw (Join-Path $OutputDirectory "$name.log")
                    if ($code -ne 101 -or $log -notmatch 'VICTORY_ORACLE_WARP debug=active' -or
                        $log -notmatch 'VICTORY_ORACLE_RG8_CONTROL guest=49 host=49' -or
                        $log -notmatch 'VICTORY_ORACLE_R4G4 guest=190 host=(absent|190)' -or
                        $log -notmatch 'R4G4 must have an RG8 host image') {
                        throw 'The baseline did not fail at the expected R4G4 host format assertion'
                    }
                } else {
                    Assert-Passed $name $code
                    Assert-OneTest $name 'VICTORY_ORACLE_R4G4 guest=190 host=49 debug_error=None'
                }
            }
        } finally {
            Pop-Location
            git worktree remove --force $tree | Out-Host
        }
    }

    if ($status -lt 0) { throw 'The D3D12 debug interface is unavailable. See debug-layer.log' }
    Assert-Passed $gpu $gpuCode
    $software = if ($Adapter -eq 'WARP') { 'true' } else { 'false' }
    Assert-OneTest 'candidate-d3d12' "adapter=.*software=$software"
    'PASS' | Set-Content (Join-Path $OutputDirectory 'graphics-status.txt')
} catch {
    $_ | Out-String | Set-Content (Join-Path $OutputDirectory 'failure.txt')
    throw
}
