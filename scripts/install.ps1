# graph-nexus 一鍵安裝（Windows PowerShell 5.1+）
#
#   irm https://github.com/coseto6125/graph-nexus/releases/latest/download/install.ps1 | iex
#   irm https://github.com/coseto6125/graph-nexus/releases/download/v0.1.0/install.ps1 | iex
#
# 環境變數：
#   $env:GNX_VERSION        指定版本（不含 v）。預設 latest。
#   $env:GNX_INSTALL_DIR    安裝目錄。預設 $env:LOCALAPPDATA\Programs\gnx。
#   $env:GNX_NO_VERIFY = 1  跳過 SHA256 驗證（不建議）。
#   $env:GNX_FORCE_CARGO=1  跳過 release binary，強制走 `cargo install --git`。
#
# 沒有 GitHub Release 或當前架構沒 prebuilt 時，會自動 fallback 到
# `cargo install --git`（需要 cargo / rustup）。

$ErrorActionPreference = 'Stop'

$repo = 'coseto6125/graph-nexus'
$bin  = 'gnx'
$version = if ($env:GNX_VERSION) { $env:GNX_VERSION } else { 'latest' }

function Invoke-CargoFallback([string]$reason) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host "error: $reason" -ForegroundColor Red
        Write-Host "       and ``cargo`` not found in PATH — install Rust from https://rustup.rs," -ForegroundColor Red
        Write-Host "       then re-run this script (or wait for a prebuilt release)." -ForegroundColor Red
        exit 1
    }
    Write-Host "==> $reason"
    Write-Host "==> Falling back to ``cargo install --git`` (source build, may take a few minutes)"
    $cargoArgs = @('install', '--git', "https://github.com/$repo", '--bin', $bin, '--locked')
    if ($script:version -ne 'latest') {
        $cargoArgs += @('--tag', "v$($script:version.TrimStart('v'))")
    }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Write-Host ""
    Write-Host "✓ Installed $bin via cargo (binary at `$env:USERPROFILE\.cargo\bin\$bin.exe)"
    exit 0
}

if ($env:GNX_FORCE_CARGO -eq '1') {
    Invoke-CargoFallback 'GNX_FORCE_CARGO=1 set'
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

# ---- 安裝目錄 ----
if (-not $env:GNX_INSTALL_DIR) {
    $installDir = Join-Path $env:LOCALAPPDATA "Programs\gnx"
} else {
    $installDir = $env:GNX_INSTALL_DIR
}
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

# ---- 下載 ----
$name    = "$bin-$tag-$target"
$archive = "$name.zip"
$url     = "https://github.com/$repo/releases/download/$tag/$archive"
$shaUrl  = "$url.sha256"

$tmp = Join-Path $env:TEMP "gnx-install-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

try {
    Write-Host "==> Downloading $bin $ver ($target)"
    Write-Host "    $url"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile (Join-Path $tmp $archive)
    } catch {
        Invoke-CargoFallback "release asset for $target not found (tag $tag)"
    }

    if ($env:GNX_NO_VERIFY -ne '1') {
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
    Write-Host "  ⚠  $installDir is not in user PATH. Add with:"
    Write-Host "       [Environment]::SetEnvironmentVariable('Path', `"$installDir;$([Environment]::GetEnvironmentVariable('Path', 'User'))`", 'User')"
    Write-Host "     Restart your shell after running."
    Write-Host ""
}

Write-Host "  Verify provenance:"
Write-Host "    gh attestation verify $installDir\$bin.exe --owner coseto6125"
