# Requires rustc and local checkouts of VIctory and bcpack-rs. The source tree
# stays read-only. Example: ./run-entropy-replay.ps1 -Victory ../VIctory -Reference ../bcpack-rs
param(
    [Parameter(Mandatory)][string]$Victory,
    [Parameter(Mandatory)][string]$Reference,
    [int]$ExpectedDisagreements = 0,
    [ValidateRange(0, 200000)][int]$AdditionalCases = 0,
    [string]$Rustc = "rustc"
)
$ErrorActionPreference = "Stop"
$refSrc = (Resolve-Path (Join-Path $Reference "src")).Path.Replace('\', '/')
$vicSrc = (Resolve-Path (Join-Path $Victory "crates/victory-formats/src")).Path.Replace('\', '/')
$work = Join-Path ([IO.Path]::GetTempPath()) ("victory-entropy-" + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $work
try {
    # bcpack-rs's two entropy modules and generated models are read from the
    # checkout. Its optional BC1 fixture convenience function needs the layout.
    $root = @"
#[path = r#"$refSrc/rans.rs"#] pub mod rans;
#[path = r#"$refSrc/tables.rs"#] pub mod tables;
#[path = r#"$refSrc/encode.rs"#] pub mod encode;
pub mod bc1 { pub const BC1_ALPHABET_BITS: [u8; 12] = [7, 7, 4, 5, 4, 4, 5, 4, 7, 7, 7, 7]; }
"@
    $refFile = Join-Path $work "reference.rs"
    [IO.File]::WriteAllText($refFile, $root)
    $referenceLib = Join-Path $work "libbcpack_reference.rlib"
    & $Rustc --edition=2021 --crate-name bcpack_reference --crate-type rlib -C opt-level=2 $refFile -o $referenceLib
    if ($LASTEXITCODE -ne 0) { throw "reference compilation failed" }

    # Append the adapter inside a temporary copy of the subject module.
    # Redirect include_bytes! to the original file so no table is distributed.
    $source = [IO.File]::ReadAllText("$vicSrc/bcpack.rs")
    $source = $source.Replace('include_bytes!("bcpack-tables.bin")', "include_bytes!(r#`"$vicSrc/bcpack-tables.bin`"#)")
    $source += @'

pub fn replay(widths: &[u8], per: usize, blocks: usize, src: &[u8], records: &mut [u8]) -> Result<(), Error> {
    static TABLES: std::sync::OnceLock<Result<Tables, Error>> = std::sync::OnceLock::new();
    let tables = TABLES.get_or_init(Tables::builtin).as_ref().map_err(|error| *error)?;
    let mut wire = src.iter().copied();
    lanes(tables, widths, per, blocks, |_| wire.next(), records)
}
'@
    $subjectFile = Join-Path $work "subject.rs"
    [IO.File]::WriteAllText($subjectFile, $source)
    $subjectLib = Join-Path $work "libvictory_subject.rlib"
    & $Rustc --edition=2024 --crate-name victory_subject --crate-type rlib -C opt-level=2 $subjectFile -o $subjectLib
    if ($LASTEXITCODE -ne 0) { throw "subject compilation failed" }

    $exe = Join-Path $work "entropy-replay.exe"
    & $Rustc --edition=2024 -C opt-level=2 --extern "bcpack_reference=$referenceLib" --extern "victory_subject=$subjectLib" (Join-Path $PSScriptRoot "entropy-replay.rs") -o $exe
    if ($LASTEXITCODE -ne 0) { throw "replay compilation failed" }
    & $exe $ExpectedDisagreements $AdditionalCases
    if ($LASTEXITCODE -ne 0) { throw "entropy replay failed" }
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}
