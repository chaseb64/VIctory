#Requires -Version 7.2
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$UpstreamBase,
    [string]$GuestLayout,
    [Parameter(Mandatory)][string]$HostCandidate,
    [string]$WaveCandidate,
    [Parameter(Mandatory)][string]$EvidenceDir,
    [Parameter(Mandatory)][string]$WorkDir,
    [ValidateSet('contributions', 'workspace')][string]$Suite = 'contributions'
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

if (!$IsWindows) { throw 'Use native Windows PowerShell 7 on an x64 Windows host.' }
$repository = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$evidence = [IO.Path]::GetFullPath($EvidenceDir)
$work = [IO.Path]::GetFullPath($WorkDir)
if (Test-Path -LiteralPath $work) { throw "Choose an unused work directory: $work" }
$null = New-Item -ItemType Directory -Path $evidence -Force
$null = New-Item -ItemType Directory -Path $work

$commits = if ($Suite -eq 'workspace') {
    [ordered]@{ baseline = $UpstreamBase; candidate = $HostCandidate }
} else {
    [ordered]@{
        upstream = $UpstreamBase
        guest = $GuestLayout
        host = $HostCandidate
        wave_original = '42bd6f544f4ff2ede49e219762a41ae5f75e0b4a'
        wave = $WaveCandidate
    }
}
$trees = @{}
foreach ($role in $commits.Keys) {
    $trees[$role] = Join-Path $work $role
}
$results = [ordered]@{}
$report = [ordered]@{
    suite = $Suite
    workflow_sha = $env:GITHUB_SHA
    os_image = $env:ImageOS
    image_version = $env:ImageVersion
    started_utc = [DateTime]::UtcNow.ToString('o')
    source_commits = $commits
    checks = $results
}

function Write-Log([string]$Line) {
    Add-Content -LiteralPath $script:Log -Value $Line -Encoding utf8
    Write-Host $Line
}

function Invoke-Logged([string]$Program, [string[]]$Arguments, [int]$ExpectedExit = 0) {
    Write-Log ("> $Program " + ($Arguments -join ' '))
    & $Program @Arguments 2>&1 |
        ForEach-Object { $_.ToString() } |
        Tee-Object -FilePath $script:Log -Append |
        ForEach-Object { Write-Host $_ }
    $code = $LASTEXITCODE
    Write-Log "exit_code=$code"
    if ($code -ne $ExpectedExit) {
        throw "$Program returned $code (expected $ExpectedExit)."
    }
}

function Invoke-Check([string]$Name, [string]$Source, [scriptblock]$Body) {
    $script:Log = Join-Path $evidence "$Name.log"
    Set-Content -LiteralPath $script:Log -Value "check=$Name source=$Source" -Encoding utf8
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $result = [ordered]@{ outcome = 'failed'; seconds = 0; log = "$Name.log" }
    $results[$Name] = $result
    try {
        Push-Location -LiteralPath $Source
        try { & $Body } finally { Pop-Location }
        $result.outcome = 'passed'
    } catch {
        $result['error'] = $_.Exception.Message
        Write-Log "FAIL $($result.error)"
    } finally {
        $result.seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 2)
        Write-Log "result=$($result.outcome) seconds=$($result.seconds)"
        $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $evidence 'run.json') -Encoding utf8
    }
}

function Get-GitLine([string[]]$Arguments) {
    $line = & git @Arguments
    if ($LASTEXITCODE -ne 0) { throw ('git ' + ($Arguments -join ' ') + ' failed.') }
    return ([string]$line).Trim()
}

function Require-Log([string]$Pattern, [string]$Details) {
    if ((Get-Content -Raw -LiteralPath $script:Log) -cnotmatch $Pattern) {
        throw $Details
    }
}

function Require-OneTest([string]$TestName, [switch]$ExpectedFailure) {
    Require-Log '(?m)^running 1 test\r?$' 'The exact test was not run once.'
    Require-Log ("(?m)^test " + [regex]::Escape($TestName) + ' \.\.\.') "Missing exact test: $TestName"
    if ($ExpectedFailure) {
        Require-Log 'test result: FAILED\. 0 passed; 1 failed; 0 ignored;' 'The exact test did not fail in libtest.'
    } else {
        Require-Log 'test result: ok\. 1 passed; 0 failed; 0 ignored;' 'The exact test did not pass in libtest.'
    }
}

function Invoke-GuestLayoutOracle([switch]$ExpectedFailure) {
    $cargoArgs = @('test', '--locked', '-p', 'victory-formats', '--lib', 'texture::tests::xbox_format_', '--', '--test-threads=1')
    $status = 'ok'
    if ($ExpectedFailure) {
        Invoke-Logged cargo $cargoArgs -ExpectedExit 101
        $status = 'FAILED'
        Require-Log 'test result: FAILED\. 0 passed; 2 failed; 0 ignored;' 'Expected two guest-layout test failures.'
        $output = Get-Content -Raw -LiteralPath $script:Log
        if ([regex]::Matches($output, '(?m)^called .*Err.*value: Invalid\r?$').Count -ne 2) {
            throw 'Both baseline tests must reach and reject the known layout descriptor.'
        }
    } else {
        Invoke-Logged cargo $cargoArgs
        Require-Log 'test result: ok\. 2 passed; 0 failed; 0 ignored;' 'Expected two passing guest-layout tests.'
    }
    foreach ($format in 189, 190) {
        Require-Log "(?m)^test texture::tests::xbox_format_$($format)_tiles_and_detiles \.\.\. $status\r?$" "Missing guest format $format result."
    }
}

function Invoke-WaveCampaign([string]$TestName) {
    $env:WAVE_SEED = '0'
    $env:WAVE_COUNT = '10000'
    Write-Log "WAVE_SEED=$env:WAVE_SEED WAVE_COUNT=$env:WAVE_COUNT"
    Invoke-Logged cargo @('test', '--release', '--locked', '-p', 'victory-shader', '--test', 'equivalence',
        $TestName, '--', '--exact', '--ignored', '--nocapture', '--test-threads=1')
    Require-OneTest $TestName
    Require-Log ('first_seed=0 .* Counts \{ attempted: 10000, ssa_agreed: 10000, both_agreed: 10000, ' +
        'refused: 0, unsupported: 0, invalid: 0, compiler_error: 0, limit: 0, crash: 0, differences: 0 \}') 'The full campaign must agree at both stages, with zero incomplete results.'
}

function Invoke-HostBoundaryOracle([switch]$ExpectedFailure) {
    $resource = Join-Path (Get-Location).Path 'crates/victory-gpu/src/resource.rs'
    $fixture = Join-Path $PSScriptRoot 'r4g4_host_boundary.rs'
    $fixtureDir = Join-Path (Split-Path $resource) 'resource'
    $null = New-Item -ItemType Directory -Path $fixtureDir -Force
    Copy-Item -LiteralPath $fixture -Destination (Join-Path $fixtureDir 'windows_validation_r4g4.rs')
    $nl = [string][char]10
    [IO.File]::AppendAllText($resource, $nl + 'include!("resource/windows_validation_r4g4.rs");' + $nl)
    Write-Log "fixture_sha256=$((Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash)"
    Invoke-Logged git @('status', '--short')
    $test = 'resource::windows_validation_r4g4::shared_r4g4_host_boundary'
    $cargoArgs = @('test', '--locked', '-p', 'victory-gpu', '--features', 'diagnostics', '--lib',
        $test, '--', '--exact', '--ignored', '--nocapture', '--test-threads=1')
    if ($ExpectedFailure) {
        Invoke-Logged cargo $cargoArgs -ExpectedExit 101
        Require-OneTest $test -ExpectedFailure
        Require-Log 'VICTORY_ORACLE_R4G4 guest=190 host=(absent|190) ' 'Missing the baseline host-format refusal.'
        Require-Log 'assertion .*failed: R4G4 must have an RG8 host image' 'The baseline did not reach the host-format assertion.'
    } else {
        Invoke-Logged cargo $cargoArgs
        Require-OneTest $test
        Require-Log 'VICTORY_ORACLE_R4G4 guest=190 host=49 debug_error=None' 'Missing the candidate typed RG8 host image.'
    }
    Require-Log 'VICTORY_ORACLE_WARP debug=active' 'Missing proof of the WARP device and active D3D12 debug queue.'
    Require-Log 'VICTORY_ORACLE_RG8_CONTROL guest=49 host=49' 'Missing the ordinary RG8 control.'
}

function Require-CleanHostStartup([string]$OracleCheck) {
    $oracleLog = Join-Path $evidence "$OracleCheck.log"
    $startup = @(Select-String -LiteralPath $oracleLog -SimpleMatch 'VICTORY_ORACLE_STARTUP debug_error=')
    if ($startup.Count -ne 1) { throw 'The host debug queue was not observed at startup.' }
    Write-Log $startup[0].Line
    Require-Log 'VICTORY_ORACLE_STARTUP debug_error=None' 'D3D12 reported a startup validation error before the format check.'
}

function Set-Target([string]$Variant) {
    $env:CARGO_TARGET_DIR = Join-Path $work "target-$Variant"
}

function Clear-Target {
    if (Test-Path -LiteralPath $env:CARGO_TARGET_DIR) {
        try {
            Remove-Item -LiteralPath $env:CARGO_TARGET_DIR -Recurse -Force
        } catch {
            Write-Warning "Could not remove validation target $env:CARGO_TARGET_DIR : $($_.Exception.Message)"
        }
    }
}

function Invoke-Workspace([string]$Role) {
    Invoke-Check "workspace-build-$Role" $trees[$Role] {
        Invoke-Logged cargo @('build', '--workspace', '--locked')
    }
    Invoke-Check "workspace-test-$Role" $trees[$Role] {
        Invoke-Logged cargo @('test', '--workspace', '--locked')
    }
}

function Invoke-WindowsToolchain([string]$Name, [string]$Source) {
    Invoke-Check $Name $Source {
        $os = Get-CimInstance Win32_OperatingSystem
        Write-Log "windows=$($os.Caption) version=$($os.Version) build=$($os.BuildNumber) image=$env:ImageOS image_version=$env:ImageVersion"
        Write-Log "architecture=$([Runtime.InteropServices.RuntimeInformation]::OSArchitecture) powershell=$($PSVersionTable.PSVersion)"
        Write-Log (Get-Content -Raw -LiteralPath 'rust-toolchain.toml')
        Invoke-Logged rustup @('toolchain', 'install')
        Invoke-Logged rustc @('-Vv')
        Require-Log '(?m)^host: x86_64-pc-windows-msvc\r?$' 'The native Windows MSVC toolchain is required.'
        Invoke-Logged cargo @('-V')
    }
}

function Require-ChecksPassed {
    $failed = @($results.Keys | Where-Object { $results[$_].outcome -eq 'failed' })
    if ($failed.Count -gt 0) { throw ('Windows checks failed: ' + ($failed -join ', ')) }
}

function Invoke-WarpReadback {
    $test = 'resource::tests::r4g4_gpu_test::sampled_r4g4_gpu_upload_copy_and_sample'
    $source = 'crates/victory-gpu/src/resource/r4g4_gpu_test.rs'
    Write-Log ('WARP=' + $env:VICTORY_D3D12_WARP + ' debug=' + $env:VICTORY_D3D12_DEBUG +
        ' require_debug=' + $env:VICTORY_REQUIRE_D3D12_DEBUG + ' DRED=' + $env:VICTORY_D3D12_DRED)
    Write-Log "readback_test_sha256=$((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash)"
    Invoke-Logged cargo @('test', '--locked', '-p', 'victory-gpu', '--features', 'diagnostics', '--lib',
        $test, '--', '--exact', '--ignored', '--nocapture', '--test-threads=1')
    Require-OneTest $test
    Require-Log 'adapter=.* vendor=[0-9a-f]+ device=[0-9a-f]+ software=true' 'The readback did not report a software adapter.'
    Write-Log 'readback_confirmed=guest-upload-copy-load-point-linear-and-watched-swizzled-refresh'
}

function Invoke-WithWarp([scriptblock]$Body) {
    $env:VICTORY_D3D12_WARP = '1'
    $env:VICTORY_D3D12_DEBUG = '1'
    $env:VICTORY_D3D12_DRED = '1'
    $env:VICTORY_REQUIRE_D3D12_DEBUG = '1'
    try { & $Body } finally {
        $env:VICTORY_D3D12_WARP = $null
        $env:VICTORY_D3D12_DEBUG = $null
        $env:VICTORY_D3D12_DRED = $null
        $env:VICTORY_REQUIRE_D3D12_DEBUG = $null
    }
}

function Set-ProcessVariable([string]$Name, $Value) {
    if ($null -eq $Value) {
        Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
    } else {
        Set-Item -LiteralPath "Env:$Name" -Value $Value
    }
}

$environment = [ordered]@{
    CARGO_TERM_COLOR = 'never'
    # The loader fixture's nested cargo expects the native target/debug layout.
    CARGO_BUILD_TARGET = $null
    CARGO_TARGET_DIR = $null
    CARGO_INCREMENTAL = '0'
    CARGO_PROFILE_DEV_DEBUG = '0'
    CARGO_PROFILE_TEST_DEBUG = '0'
    WAVE_SEED = $null
    WAVE_COUNT = $null
    VICTORY_D3D12_WARP = $null
    VICTORY_D3D12_DEBUG = $null
    VICTORY_D3D12_DRED = $null
    VICTORY_D3D12_GBV = $null
    VICTORY_REQUIRE_D3D12_DEBUG = $null
}
$previousEnvironment = @{}
foreach ($key in $environment.Keys) {
    $previousEnvironment[$key] = [Environment]::GetEnvironmentVariable($key)
    Set-ProcessVariable $key $environment[$key]
}

try {
    Invoke-Check 'source-identity' $repository {
        Invoke-Logged git @('show', '-s', '--format=controller=%H tree=%T', 'HEAD')
        foreach ($role in $commits.Keys) {
            $sha = $commits[$role]
            if ($sha -cnotmatch '^[0-9a-f]{40}$') { throw "$role must be a full lowercase commit SHA." }
            if ((Get-GitLine @('rev-parse', '--verify', "$sha^{commit}")) -cne $sha) {
                throw "$role must resolve to that exact commit in the fork checkout."
            }
            Write-Log "source_role=$role"
            Invoke-Logged git @('show', '-s', '--format=%H tree=%T parents=%P %s', $sha)
        }
        if ($Suite -eq 'workspace') { return }
        if ((Get-GitLine @('rev-parse', "$GuestLayout^")) -cne $UpstreamBase) {
            throw 'The guest layout commit must be a direct child of the upstream base.'
        }
        if ($GuestLayout -ceq $HostCandidate) {
            throw 'The sampled R4G4 candidate must differ from the guest layout parent.'
        }
        Invoke-Logged git @('merge-base', '--is-ancestor', $UpstreamBase, $commits.wave_original)
        Invoke-Logged git @('merge-base', '--is-ancestor', $GuestLayout, $HostCandidate)
        Invoke-Logged git @('merge-base', '--is-ancestor', $commits.wave_original, $WaveCandidate)
    }
    if ($results['source-identity'].outcome -ne 'passed') { throw 'The source identities could not be verified.' }

    foreach ($role in $commits.Keys) {
        Invoke-Check "checkout-$role" $repository {
            Invoke-Logged git @('worktree', 'add', '--detach', $trees[$role], $commits[$role])
            if ((Get-GitLine @('-C', $trees[$role], 'rev-parse', 'HEAD')) -cne $commits[$role]) {
                throw "Wrong checkout for $role."
            }
        }
    }

    if ($Suite -eq 'workspace') {
        foreach ($role in $commits.Keys) {
            Invoke-WindowsToolchain "windows-toolchain-$role" $trees[$role]
            Set-Target $role
            Invoke-Workspace $role
            Clear-Target
        }
        Require-ChecksPassed
        return
    }

    Invoke-WindowsToolchain 'windows-toolchain' $trees.host
    Set-Target 'host'
    Invoke-WithWarp {
        Invoke-Check 'r4g4-warp-readback' $trees.host { Invoke-WarpReadback }
        if ($results['r4g4-warp-readback'].outcome -eq 'failed') {
            $env:VICTORY_D3D12_DEBUG = $null
            $env:VICTORY_REQUIRE_D3D12_DEBUG = $null
            Invoke-Check 'r4g4-warp-readback-no-debug' $trees.host { Invoke-WarpReadback }
        }
    }
    Invoke-Workspace 'host'
    Clear-Target

    Set-Target 'guest'
    Invoke-Workspace 'guest'
    Invoke-Check 'guest-layout-candidate' $trees.guest { Invoke-GuestLayoutOracle }
    Clear-Target

    Set-Target 'upstream-with-guest-oracle'
    Invoke-Check 'guest-layout-baseline' $trees.upstream {
        $relative = 'crates/victory-formats/src/texture.rs'
        $nl = [string][char]10
        $crlf = [string][char]13 + $nl
        $candidate = [IO.File]::ReadAllText((Join-Path $trees.guest $relative)).Replace($crlf, $nl)
        $destination = Join-Path $trees.upstream $relative
        $baseline = [IO.File]::ReadAllText($destination).Replace($crlf, $nl)
        $start = '    #[test]' + $nl + '    fn xbox_format_189_tiles_and_detiles()'
        $end = '    #[test]' + $nl + '    fn texture_copies_reject_overflowing_pitches()'
        $startAt = $candidate.IndexOf($start, [StringComparison]::Ordinal)
        $endAt = $candidate.IndexOf($end, [StringComparison]::Ordinal)
        $destinationAt = $baseline.IndexOf($end, [StringComparison]::Ordinal)
        if ($startAt -lt 0 -or $endAt -le $startAt -or $destinationAt -lt 0 -or $baseline.Contains($start)) {
            throw 'Guest oracle insertion anchors changed.'
        }
        $fixture = $candidate.Substring($startAt, $endAt - $startAt)
        $fixtureFile = Join-Path $evidence 'guest-layout-oracle.txt'
        [IO.File]::WriteAllText($fixtureFile, $fixture)
        Write-Log "fixture_sha256=$((Get-FileHash -LiteralPath $fixtureFile -Algorithm SHA256).Hash)"
        [IO.File]::WriteAllText($destination, $baseline.Insert($destinationAt, $fixture))
        Invoke-Logged git @('diff', '--', $relative)
        Invoke-GuestLayoutOracle -ExpectedFailure
    }
    Clear-Target

    Set-Target 'wave-original'
    Invoke-Check 'wave32-original' $trees.wave_original { Invoke-WaveCampaign 'search::campaign' }
    Clear-Target
    Set-Target 'wave'
    Invoke-Check 'wave32-suite' $trees.wave { Invoke-Logged cargo @('test', '--locked', '-p', 'victory-shader') }
    Invoke-Check 'wave32-candidate-legacy' $trees.wave { Invoke-WaveCampaign 'search::campaign' }
    Invoke-Check 'wave32-candidate-targeted' $trees.wave { Invoke-WaveCampaign 'search::targeted_campaign' }
    Clear-Target

    Invoke-WithWarp {
        Set-Target 'guest-with-r4g4-oracle'
        Invoke-Check 'r4g4-host-baseline' $trees.guest { Invoke-HostBoundaryOracle -ExpectedFailure }
        Invoke-Check 'r4g4-host-baseline-startup-debug' $trees.guest { Require-CleanHostStartup 'r4g4-host-baseline' }
        Clear-Target
        Set-Target 'host-with-r4g4-oracle'
        Invoke-Check 'r4g4-host-candidate' $trees.host { Invoke-HostBoundaryOracle }
        Invoke-Check 'r4g4-host-candidate-startup-debug' $trees.host { Require-CleanHostStartup 'r4g4-host-candidate' }
        Clear-Target
    }

    Require-ChecksPassed
} finally {
    $summary = @("## Windows validation ($Suite)", '', '| Check | Result | Evidence |', '| --- | --- | --- |')
    foreach ($name in $results.Keys) {
        $result = $results[$name]
        $summary += "| $name | $($result.outcome) | $($result.log) |"
    }
    $summary | Set-Content -LiteralPath (Join-Path $evidence 'summary.md') -Encoding utf8
    if ($env:GITHUB_STEP_SUMMARY) {
        $summary | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Encoding utf8
    }
    foreach ($key in $environment.Keys) {
        Set-ProcessVariable $key $previousEnvironment[$key]
    }
}
