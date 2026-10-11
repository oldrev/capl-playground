param(
  [int]$Port = 8080
)

Push-Location $PSScriptRoot
try {

if (-not (Get-Command trunk -ErrorAction SilentlyContinue)) {
  Write-Error "Trunk is required. Install it once with: cargo install --locked trunk"
  exit 1
}

$env:RUSTUP_TOOLCHAIN = "nightly"
$target = rustup target list --installed --toolchain nightly
if (-not ($target | Select-String "wasm32-unknown-unknown")) {
  Write-Error "The nightly WASM target is required. Install it once with: rustup +nightly target add wasm32-unknown-unknown"
  exit 1
}

trunk serve crates/capl-web/index.html --port $Port --open
} finally {
  Pop-Location
}
