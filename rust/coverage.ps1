$ErrorActionPreference = "Stop"

Write-Host "Running Rust tests with LLVM coverage..."
New-Item -ItemType Directory -Force coverage | Out-Null
cargo llvm-cov --workspace --all-features --lcov --output-path coverage/lcov.info
cargo llvm-cov report --summary-only
Write-Host "LCOV report: rust/coverage/lcov.info"
