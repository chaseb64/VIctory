# Native Windows validation

Push a reviewed candidate with this workflow to a `validation/windows-*` branch in a public fork. The push launches both jobs on standard Windows runners. Each job uploads its logs even when a check fails. Use a new validation branch or rerun the existing workflow to repeat testing. GitHub exposes the manual trigger once the workflow exists on the fork's default branch.

The Checks suite runs the R4G4 CPU regressions, workspace tests, both Clippy configurations and the asset policy. The R4G4 suite runs the candidate's existing shader test and applies the same small resource-creation oracle to two temporary Git worktrees. The baseline control requires an ordinary RG8 texture to succeed on the same debug-enabled WARP device before accepting the expected failure from guest format 190. The candidate must create a typed RG8 host texture and run the shader test. Run a future candidate's Checks suite independently or supply its baseline commit with `-Baseline` if it uses this oracle.

Run locally from a native x64 Windows checkout with PowerShell 7 and the repository's Rust toolchain. Install Windows Graphics Tools first if the D3D12 debug layer is missing.

```powershell
./scripts/windows-validation/run.ps1 -Suite Checks -OutputDirectory "$env:TEMP/victory-checks"
./scripts/windows-validation/run.ps1 -Suite R4G4 -Adapter WARP -OutputDirectory "$env:TEMP/victory-warp"
./scripts/windows-validation/run.ps1 -Suite R4G4 -Adapter Physical -OutputDirectory "$env:TEMP/victory-hardware"
```

The hardware mode runs the shader test on the physical adapter selected by VIctory. The WARP mode also checks adapter LUID identity and runs the matched baseline and candidate oracle. Both modes require a live device and an active debug layer. The logs record adapter and driver information. Generated logs, executables and game assets stay outside the checked-in validation tree.
