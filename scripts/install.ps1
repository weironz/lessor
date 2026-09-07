[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "lessor\bin"),
    [string]$Repository = "weironz/lessor"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($env:PROCESSOR_ARCHITECTURE -notin @("AMD64", "x86_64")) {
    throw "目前仅提供 x86_64 Windows CLI。"
}

if ($Version -eq "latest") {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repository/releases/latest" -Headers @{ "User-Agent" = "lessor-installer" }
    $tag = $release.tag_name
    if ([string]::IsNullOrWhiteSpace($tag)) { throw "无法确定最新发行版本。" }
    $Version = $tag.TrimStart("v")
} else {
    $Version = $Version.TrimStart("v")
    $tag = "v$Version"
}

$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("lessor-" + [guid]::NewGuid())
$archive = Join-Path $tempDir "lessor.zip"
$extractDir = Join-Path $tempDir "extract"
$url = "https://github.com/$Repository/releases/download/$tag/lessor-$Version-x86_64-windows.zip"

try {
    New-Item -ItemType Directory -Path $extractDir -Force | Out-Null
    Write-Host "下载 lessor CLI $tag…"
    Invoke-WebRequest -Uri $url -OutFile $archive
    Expand-Archive -Path $archive -DestinationPath $extractDir -Force

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    foreach ($binary in @("lessord.exe", "lessor-netcfg.exe", "lessorctl.exe")) {
        $source = Join-Path $extractDir $binary
        if (-not (Test-Path -LiteralPath $source)) { throw "发行包缺少 $binary。" }
        Copy-Item -LiteralPath $source -Destination (Join-Path $InstallDir $binary) -Force
    }
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$pathEntries = @($userPath -split ';' | Where-Object { $_ })
if ($pathEntries -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable("Path", (($pathEntries + $InstallDir) -join ';'), "User")
    $env:Path = "$InstallDir;$env:Path"
    Write-Host "已加入当前用户 PATH；请重新打开终端以供其他终端使用。"
}

Write-Host "已安装 lessor CLI $tag 到 $InstallDir"
