[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$CacheRoot,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$InstallRoot,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EnvironmentFile,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$PathFile,

  [switch]$ValidateArgumentsOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($ValidateArgumentsOnly) {
  [ordered]@{
    cacheRoot = $CacheRoot
    installRoot = $InstallRoot
    environmentFile = $EnvironmentFile
    pathFile = $PathFile
  } | ConvertTo-Json -Compress
  return
}

$llvmVersion = "19.1.7"
$llvmArchiveName = "clang+llvm-$llvmVersion-x86_64-pc-windows-msvc.tar.xz"
$llvmArchiveSha256 = "b4557b4f012161f56a2f5d9e877ab9635cafd7a08f7affe14829bd60c9d357f0"
$llvmUrl = "https://github.com/llvm/llvm-project/releases/download/llvmorg-$llvmVersion/clang%2Bllvm-$llvmVersion-x86_64-pc-windows-msvc.tar.xz"

function Get-PinnedArchive {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Destination,

    [Parameter(Mandatory = $true)]
    [string]$Url,

    [Parameter(Mandatory = $true)]
    [string]$Sha256,

    [Parameter(Mandatory = $true)]
    [string]$Description
  )

  if ((Test-Path $Destination -PathType Leaf) -and
      (Get-FileHash -Algorithm SHA256 $Destination).Hash.ToLowerInvariant() -ne $Sha256) {
    Remove-Item -LiteralPath $Destination -Force
  }
  if (-not (Test-Path $Destination -PathType Leaf)) {
    $downloadArguments = @(
      "--fail",
      "--location",
      "--retry", "5",
      "--retry-delay", "5",
      "--retry-all-errors",
      "--continue-at", "-",
      "--output", $Destination,
      $Url
    )
    & curl.exe @downloadArguments
    if ($LASTEXITCODE -ne 0) {
      throw "could not download $Description"
    }
  }

  $actual = (Get-FileHash -Algorithm SHA256 $Destination).Hash.ToLowerInvariant()
  if ($actual -ne $Sha256) {
    throw "$Description SHA-256 mismatch: $actual"
  }
}

New-Item -ItemType Directory -Force -Path $CacheRoot | Out-Null
$llvmArchive = Join-Path $CacheRoot $llvmArchiveName
Get-PinnedArchive `
  -Destination $llvmArchive `
  -Url $llvmUrl `
  -Sha256 $llvmArchiveSha256 `
  -Description "LLVM archive"

New-Item -ItemType Directory -Force -Path $InstallRoot | Out-Null
$unpackArguments = @(
  "-xf", $llvmArchive,
  "-C", $InstallRoot,
  "--strip-components=1"
)
& tar.exe @unpackArguments
if ($LASTEXITCODE -ne 0) {
  throw "could not unpack the official LLVM development archive"
}

$llvmConfig = Join-Path $InstallRoot "bin\llvm-config.exe"
$clang = Join-Path $InstallRoot "bin\clang.exe"
$version = (& $llvmConfig --version).Trim()
if ($LASTEXITCODE -ne 0 -or $version -ne $llvmVersion) {
  throw "unexpected llvm-config version: $version"
}
$sharedMode = (& $llvmConfig --shared-mode).Trim()
$buildMode = (& $llvmConfig --build-mode).Trim()
$compilerFlags = (& $llvmConfig --cxxflags).Trim()
Write-Host "LLVM shared mode: $sharedMode"
Write-Host "LLVM build mode: $buildMode"
Write-Host "LLVM C++ flags: $compilerFlags"
$llvmCDll = Join-Path $InstallRoot "bin\LLVM-C.dll"
$llvmCImportLibrary = Join-Path $InstallRoot "lib\LLVM-C.lib"
$llvmLicense = Join-Path $InstallRoot "include\llvm\Support\LICENSE.TXT"
if (-not (Test-Path $llvmCDll -PathType Leaf) -or (Get-Item $llvmCDll).Length -eq 0) {
  throw "the official LLVM archive is missing LLVM-C.dll"
}
if (-not (Test-Path $llvmCImportLibrary -PathType Leaf) -or
    (Get-Item $llvmCImportLibrary).Length -eq 0) {
  throw "the official LLVM archive is missing LLVM-C.lib"
}
Write-Host "LLVM-C.dll: $((Get-Item $llvmCDll).Length) bytes"
Write-Host "LLVM-C.lib: $((Get-Item $llvmCImportLibrary).Length) bytes"
if (-not (Test-Path $llvmLicense -PathType Leaf) -or (Get-Item $llvmLicense).Length -eq 0) {
  throw "the official LLVM archive is missing include\llvm\Support\LICENSE.TXT"
}
$targetsBuilt = (& $llvmConfig --targets-built).Trim() -split '\s+' | Sort-Object -Unique
$expectedTargets = @("AArch64", "ARM", "X86")
if ($LASTEXITCODE -ne 0 -or
    @(Compare-Object -ReferenceObject $expectedTargets -DifferenceObject $targetsBuilt).Count -ne 0) {
  throw "unexpected LLVM target-library set: $($targetsBuilt -join ' ')"
}
& $clang --version
if ($LASTEXITCODE -ne 0) {
  throw "could not execute the pinned Clang compiler"
}

if (-not (Test-Path (Join-Path $InstallRoot "include\llvm-c\Core.h") -PathType Leaf)) {
  throw "LLVM C API development headers are incomplete"
}

Add-Content -LiteralPath $EnvironmentFile -Value "LLVM_PATH=$InstallRoot"
Add-Content -LiteralPath $EnvironmentFile -Value "LLVM_SYS_191_PREFIX=$InstallRoot"
Add-Content -LiteralPath $EnvironmentFile -Value "LOOM_CC=$clang"
Add-Content -LiteralPath $PathFile -Value (Join-Path $InstallRoot "bin")
