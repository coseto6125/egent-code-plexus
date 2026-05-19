# code-graph-nexus 一鍵安裝（Windows PowerShell 5.1+）
#
#   irm https://github.com/coseto6125/code-graph-nexus/releases/latest/download/install.ps1 | iex
#   irm https://github.com/coseto6125/code-graph-nexus/releases/download/v0.2.0/install.ps1 | iex
#
# 環境變數：
#   $env:CGN_VERSION        指定版本（不含 v）。預設 latest。
#   $env:CGN_INSTALL_DIR    安裝目錄。預設 $env:LOCALAPPDATA\Programs\cgn。
#   $env:CGN_NO_VERIFY = 1  跳過 SHA256 驗證（不建議）。
#   $env:CGN_FORCE_CARGO=1  跳過 release binary，強制走 `cargo install --git`。
#
# 沒有 GitHub Release 或當前架構沒 prebuilt 時，會自動 fallback 到
# `cargo install --git`（需要 cargo / rustup）。

$ErrorActionPreference = 'Stop'

$repo = 'coseto6125/code-graph-nexus'
$bin  = 'cgn'
$version = if ($env:CGN_VERSION) { $env:CGN_VERSION } else { 'latest' }

# ---- 安裝目錄 ---- (resolved up-front so cargo fallback respects it too)
if (-not $env:CGN_INSTALL_DIR) {
    $installDir = Join-Path $env:LOCALAPPDATA "Programs\cgn"
} else {
    $installDir = $env:CGN_INSTALL_DIR
}

function Invoke-CargoFallback([string]$reason) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host "error: $reason" -ForegroundColor Red
        Write-Host "       and ``cargo`` not found in PATH — install Rust from https://rustup.rs," -ForegroundColor Red
        Write-Host "       then re-run this script (or wait for a prebuilt release)." -ForegroundColor Red
        exit 1
    }
    Write-Host "==> $reason"
    Write-Host "==> Falling back to ``cargo install --git`` (source build, may take a few minutes)"

    # Build into a private --root then move the .exe to $installDir so the
    # cargo fallback honors CGN_INSTALL_DIR (the workspace package name is
    # required now that more than one bin exists in the workspace).
    $buildRoot = Join-Path $env:TEMP ([System.IO.Path]::GetRandomFileName())
    try {
        $cargoArgs = @('install', '--root', $buildRoot, '--git', "https://github.com/$repo", 'code-graph-nexus', '--bin', $bin, '--locked')
        if ($script:version -ne 'latest') {
            $cargoArgs += @('--tag', "v$($script:version.TrimStart('v'))")
        }
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        New-Item -ItemType Directory -Force -Path $script:installDir | Out-Null
        Copy-Item "$buildRoot\bin\$bin.exe" "$script:installDir\$bin.exe" -Force
    } finally {
        if (Test-Path $buildRoot) { Remove-Item -Recurse -Force $buildRoot }
    }
    Write-Host ""
    Write-Host "✓ Installed $bin via cargo → $script:installDir\$bin.exe"
    exit 0
}

if ($env:CGN_FORCE_CARGO -eq '1') {
    Invoke-CargoFallback 'CGN_FORCE_CARGO=1 set'
}

# ---- 偵測 ARCH → target triple ----
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
switch ($arch) {
    'X64'   { $target = 'x86_64-pc-windows-msvc' }
    default {
        Invoke-CargoFallback "unsupported prebuilt architecture: $arch (only x86_64-pc-windows-msvc has prebuilt binaries)"
    }
}

# ---- 解析版本 ----
if ($version -eq 'latest') {
    $tag = $null
    try {
        $resp = Invoke-WebRequest -UseBasicParsing -MaximumRedirection 0 -ErrorAction SilentlyContinue `
            -Uri "https://github.com/$repo/releases/latest"
        $loc = $resp.Headers.Location
        if (-not $loc) {
            $loc = (Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/$repo/releases/latest").BaseResponse.RequestMessage.RequestUri.AbsoluteUri
        }
        if ($loc -match '/tag/') {
            $tag = ($loc -split '/tag/')[-1]
        }
    } catch {
        # 落到下方 fallback
    }
    if (-not $tag) {
        Invoke-CargoFallback "no published GitHub Release yet for $repo"
    }
} else {
    $tag = "v$($version.TrimStart('v'))"
}
$ver = $tag.TrimStart('v')

# ---- 預處理：檢查進程佔用 ----
try {
    $runningCgn = Get-Process -Name $bin -ErrorAction SilentlyContinue
    if ($runningCgn) {
        Write-Host "==> Found running $bin process. Attempting to stop..." -ForegroundColor Yellow
        $runningCgn | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 1 # 等待 OS 釋放文件鎖
    }
} catch {
    Write-Host "warning: could not stop running $bin process automatically." -ForegroundColor Gray
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null

# ---- 下載 ----
$name    = "$bin-$tag-$target"
$archive = "$name.zip"
$url     = "https://github.com/$repo/releases/download/$tag/$archive"
$shaUrl  = "$url.sha256"

$tmp = Join-Path $env:TEMP "cgn-install-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

try {
    Write-Host "==> Downloading $bin $ver ($target)"
    Write-Host "    $url"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile (Join-Path $tmp $archive)
    } catch {
        Invoke-CargoFallback "release asset for $target not found (tag $tag)"
    }

    if ($env:CGN_NO_VERIFY -ne '1') {
        Invoke-WebRequest -UseBasicParsing -Uri $shaUrl -OutFile (Join-Path $tmp "$archive.sha256")
        Write-Host "==> Verifying SHA256"
        $expected = (Get-Content (Join-Path $tmp "$archive.sha256") -Raw).Trim().Split()[0].ToLower()
        $actual   = (Get-FileHash (Join-Path $tmp $archive) -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) {
            Write-Error "SHA256 mismatch: expected $expected, got $actual"
            exit 1
        }
    }

    # ---- 解壓 + 安裝 ----
    Expand-Archive -Force -Path (Join-Path $tmp $archive) -DestinationPath $tmp
    Copy-Item -Force (Join-Path $tmp "$name\$bin.exe") (Join-Path $installDir "$bin.exe")
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
}

Write-Host ""
Write-Host "✓ Installed $bin $ver → $installDir\$bin.exe"
Write-Host ""

# ---- PATH 提示 ----
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath -or -not ($userPath -split ';' | Where-Object { $_ -ieq $installDir })) {
    $newUserPath = if ($userPath) { "$installDir;$userPath" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
    $env:Path = "$installDir;$env:Path"
    Write-Host "  Added $installDir to user PATH."
    Write-Host "  It is available in this PowerShell session; restart other shells to pick it up."
    Write-Host ""
}

Write-Host "  Verify provenance:"
Write-Host "    gh attestation verify $installDir\$bin.exe --owner coseto6125"
