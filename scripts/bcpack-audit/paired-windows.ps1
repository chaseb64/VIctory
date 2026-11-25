param(
    [Parameter(Mandatory = $true)] [string] $BaselineRoot,
    [Parameter(Mandatory = $true)] [string] $CandidateRoot,
    [Parameter(Mandatory = $true)] [string] $HarnessRoot,
    [Parameter(Mandatory = $true)] [string] $EvidenceDir
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$auditLabels = @(
    'dstorage bcpack audit: open synthetic file',
    'dstorage bcpack audit: release file after completion',
    'dstorage bcpack audit: pending status is isolated across queues',
    'dstorage bcpack audit: reused status resets to pending',
    'dstorage bcpack audit: memory BCPack from a declared prefix',
    'dstorage bcpack audit: short zlib intermediate fails before destination writes',
    'dstorage bcpack audit: memory zlib then BCPack with exact intermediate',
    'dstorage bcpack audit: normal-priority file BCPack subrange',
    'dstorage bcpack audit: low-priority file zlib then BCPack with spare intermediate capacity',
    'dstorage bcpack audit: repeated BCPack on the same queue and status entry',
    'dstorage bcpack audit: single-block entropy lane completion'
)
$expectedBaselineFailures = @(
    'dstorage bcpack audit: single-block entropy lane completion',
    'xasync: an async call with no queue and no process queue is E_NO_TASK_QUEUE',
    'xsystem: a retail console id is the documented zero id',
    'dstorage: a queue filled to half its capacity is not submitted on its own',
    'xuser: XUserFindUserByLocalId of no user is HRESULT_FROM_WIN32(ERROR_NOT_FOUND)',
    'xuser: an invalid gamertag component is E_INVALIDARG'
)
$knownFailures = @(
    'xasync: an async call with no queue and no process queue is E_NO_TASK_QUEUE',
    'xsystem: a retail console id is the documented zero id',
    'dstorage: a queue filled to half its capacity is not submitted on its own',
    'xuser: XUserFindUserByLocalId of no user is HRESULT_FROM_WIN32(ERROR_NOT_FOUND)',
    'xuser: an invalid gamertag component is E_INVALIDARG'
)

function Copy-Source([string] $Source, [string] $Destination) {
    if (-not (Test-Path (Join-Path $Source 'Cargo.toml'))) {
        throw "No Cargo.toml at source root: $Source"
    }
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    & robocopy $Source $Destination /E /NFL /NDL /NJH /NJS /NP /XD .git target .scratch .codex .aws .agents /XF .git | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy failed for $Source (exit $LASTEXITCODE)" }
}

function Stage-Harness([string] $Harness, [string] $Destination) {
    $files = @(
        'crates/victory-hle/tests/conformance.rs',
        'docs/acceptance.md',
        'tests/fixtures/conformance'
    )
    foreach ($relative in $files) {
        $from = Join-Path $Harness $relative
        if (-not (Test-Path $from)) { throw "Pinned #2057 harness file is missing: $from" }
        $to = Join-Path $Destination $relative
        New-Item -ItemType Directory -Force -Path (Split-Path $to) | Out-Null
        Copy-Item -Recurse -Force $from $to
    }
}

function Invoke-PairedRun([string] $Name, [string] $Source, [string] $WorkDir) {
    $root = Join-Path $WorkDir $Name
    $target = Join-Path $root 'target'
    $logPath = Join-Path $EvidenceDir "$Name.log"
    Copy-Source $Source $root
    Stage-Harness $HarnessRoot $root

    # Extend the existing #2057 fixture at its queue teardown. Keep its own
    # TAP expectations and guest loader intact on both source revisions.
    $file = Join-Path $root 'tests/fixtures/conformance/src/dstorage.rs'
    $original = [IO.File]::ReadAllText($file).Replace("`r`n", "`n")
    $hook = '    files(t, &s, array);'
    if ([regex]::Matches($original, [regex]::Escape($hook)).Count -ne 1) {
        throw 'The pinned #2057 DirectStorage fixture hook is missing or ambiguous'
    }
    $module = '#[path = "dstorage_bcpack.rs"]' + "`nmod bcpack;`n"
    $extended = $original.Replace($hook, $hook + "`n    bcpack::run(t, &s, array);") + "`n" + $module
    [IO.File]::WriteAllText($file, $extended)
    Copy-Item (Join-Path $PSScriptRoot 'dstorage_bcpack.rs') (Split-Path $file)

    Push-Location $root
    try {
        $env:CARGO_TARGET_DIR = $target
        $output = @(& cargo test --locked -p victory-hle --test conformance the_conformance_title_runs_through_victory -- --exact --nocapture 2>&1)
        $cargoExit = $LASTEXITCODE
        $output | Set-Content -Encoding utf8 $logPath
    }
    finally {
        Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
        Pop-Location
    }

    $text = (Get-Content -Raw $logPath) -replace "`r`n", "`n"
    $reports = [regex]::Matches($text, '(?ms)^TAP version 13\n1\.\.\d+\n.*?^# failed \d+\s*$')
    if ($reports.Count -lt 1 -or $reports.Count -gt 2) { throw "${Name}: expected the TAP report once, optionally echoed once on panic, in $logPath" }
    $report = $reports[0].Value.TrimEnd()
    if ($reports.Count -eq 2 -and $reports[1].Value.TrimEnd() -cne $report) {
        throw "${Name}: second TAP report is not an identical panic echo; inspect $logPath"
    }
    $planMatches = [regex]::Matches($report, '(?m)^1\.\.(\d+)\s*$')
    $asserts = [regex]::Matches($report, '(?m)^(ok|not ok) (\d+) - (.+)$')
    if ($planMatches.Count -ne 1) { throw "${Name}: expected one complete TAP plan in $logPath; inspect retained log" }
    $plan = [int]$planMatches[0].Groups[1].Value
    if ($asserts.Count -ne $plan) { throw "${Name}: TAP has $($asserts.Count) results for plan 1..$plan; inspect retained log" }
    for ($i = 0; $i -lt $asserts.Count; $i++) {
        if ([int]$asserts[$i].Groups[2].Value -ne ($i + 1)) { throw "${Name}: TAP result numbering is not contiguous" }
        if ($asserts[$i].Groups[3].Value -match '# SKIP') { throw "${Name}: TAP contains a skipped assertion" }
    }

    $seenAudit = @{}
    foreach ($match in $asserts) {
        $label = $match.Groups[3].Value.TrimEnd()
        foreach ($auditLabel in $auditLabels) {
            if ($label -ceq $auditLabel) {
                if ($seenAudit.ContainsKey($auditLabel)) { throw "${Name}: duplicate TAP marker '$auditLabel'" }
                $seenAudit[$auditLabel] = $match.Groups[1].Value
            }
        }
    }
    foreach ($auditLabel in $auditLabels) {
        if (-not $seenAudit.ContainsKey($auditLabel)) { throw "${Name}: missing TAP marker '$auditLabel'" }
    }

    Write-Host "$Name cargo exit: $cargoExit; complete TAP plan: $plan; audit markers: $($auditLabels.Count)"
    $failureRows = @($asserts | Where-Object { $_.Groups[1].Value -eq 'not ok' } | ForEach-Object { $_.Groups[3].Value.TrimEnd() })
    $failureCount = [int]([regex]::Match($report, '(?m)^# failed (\d+)\s*$').Groups[1].Value)
    if ($failureCount -ne $failureRows.Count) { throw "${Name}: TAP summary count $failureCount differs from $($failureRows.Count) not-ok rows" }
    $allFailures = @($failureRows | Sort-Object -Unique)
    if ($Name -eq 'baseline') {
        $actual = $allFailures
        $expected = @($expectedBaselineFailures | Sort-Object -Unique)
        if ($failureRows.Count -ne $expectedBaselineFailures.Count -or ($actual -join "`n") -cne ($expected -join "`n")) {
            throw "Baseline red markers differ. Expected [$($expected -join '; ')]; got [$($actual -join '; ')]. Log: $logPath"
        }
        $starvationLabel = [regex]::Escape('dstorage bcpack audit: single-block entropy lane completion')
        if ($report -notmatch "(?m)^not ok \d+ - $starvationLabel\n\s+# HRESULT 0x8007000d\b") {
            throw "Baseline starvation failure did not report expected HRESULT 0x8007000d. Log: $logPath"
        }
        if ($cargoExit -ne 101) { throw "Expected the baseline Cargo assertion failure (101), got $cargoExit. Log: $logPath" }
        Write-Host 'Baseline red set matches the five known divergences plus the expected starvation failure.'
    }
    else {
        $expected = @($knownFailures | Sort-Object -Unique)
        if ($failureRows.Count -ne $knownFailures.Count -or ($allFailures -join "`n") -cne ($expected -join "`n")) {
            throw "Candidate failures differ from the five pinned known divergences. Got [$($allFailures -join '; ')]. Log: $logPath"
        }
        if ($cargoExit -ne 0) { throw "Candidate cargo test failed despite green TAP markers. Log: $logPath" }
        Write-Host 'Candidate: all 11 distinct audit markers passed.'
    }
}

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$BaselineRoot = (Resolve-Path $BaselineRoot).Path
$CandidateRoot = (Resolve-Path $CandidateRoot).Path
$HarnessRoot = (Resolve-Path $HarnessRoot).Path
if (Test-Path $EvidenceDir) {
    if ((Get-ChildItem -Force $EvidenceDir | Measure-Object).Count -ne 0) {
        throw "EvidenceDir must be new or empty to preserve prior logs: $EvidenceDir"
    }
} else {
    New-Item -ItemType Directory -Path $EvidenceDir | Out-Null
}
$EvidenceDir = (Resolve-Path $EvidenceDir).Path
$pairedWorkDir = Join-Path $env:TEMP ("bcpack-paired-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $pairedWorkDir | Out-Null

Invoke-PairedRun 'baseline' $BaselineRoot $pairedWorkDir
Invoke-PairedRun 'candidate' $CandidateRoot $pairedWorkDir
Write-Host "Paired Windows validation passed. Logs retained at $EvidenceDir; isolated builds retained at $pairedWorkDir"
