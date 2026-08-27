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

$libxmlVersion = "2.9.12"
$libxmlArchiveName = "libxml2-v$libxmlVersion.tar.gz"
$libxmlArchiveSha256 = "98bfa7a9a5e2a75638422050740448ee9f02bf4dc2075c9822d7747d5ff9e617"
$libxmlUrl = "https://gitlab.gnome.org/GNOME/libxml2/-/archive/v$libxmlVersion/libxml2-v$libxmlVersion.tar.gz"

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
Write-Host "LLVM-C.dll present: $(Test-Path $llvmCDll -PathType Leaf)"
Write-Host "LLVM-C.lib present: $(Test-Path $llvmCImportLibrary -PathType Leaf)"
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

$llvmLibraries = (& $llvmConfig --link-static --libfiles).Trim()
if ($LASTEXITCODE -ne 0 -or
    [string]::IsNullOrWhiteSpace($llvmLibraries) -or
    -not (Test-Path (Join-Path $InstallRoot "include\llvm-c\Core.h") -PathType Leaf)) {
  throw "LLVM development libraries are incomplete"
}
foreach ($library in ($llvmLibraries -split '\s+')) {
  if (-not (Test-Path $library -PathType Leaf)) {
    throw "llvm-config reported a missing library: $library"
  }
}

$systemLibraries = (& $llvmConfig --link-static --system-libs).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($systemLibraries)) {
  throw "llvm-config did not report its static system-library closure"
}
$systemLibraryNames = $systemLibraries -split '\s+'
if ($systemLibraryNames -notcontains "libxml2s.lib") {
  throw "the pinned LLVM package no longer reports its expected libxml2s.lib dependency"
}

$libxmlLibrary = Join-Path $InstallRoot "lib\libxml2s.lib"
if (-not (Test-Path $libxmlLibrary -PathType Leaf)) {
  # LLVM's Windows release recipe links this exact static libxml2 build, but
  # the published development archive omits it.
  $libxmlArchive = Join-Path $CacheRoot $libxmlArchiveName
  Get-PinnedArchive `
    -Destination $libxmlArchive `
    -Url $libxmlUrl `
    -Sha256 $libxmlArchiveSha256 `
    -Description "libxml2 source archive"

  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  $vsRoot = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
  if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($vsRoot)) {
    throw "could not locate the Visual C++ x86-64 toolchain"
  }
  $vsDevCmd = Join-Path $vsRoot "Common7\Tools\VsDevCmd.bat"
  $devCommand = "`"$vsDevCmd`" -no_logo -arch=x64 -host_arch=x64 && set"
  $developerEnvironment = & cmd.exe /d /s /c $devCommand
  if ($LASTEXITCODE -ne 0) {
    throw "could not initialize the Visual C++ x86-64 toolchain"
  }
  foreach ($entry in $developerEnvironment) {
    $separator = $entry.IndexOf('=')
    if ($separator -gt 0) {
      Set-Item -Path "Env:$($entry.Substring(0, $separator))" -Value $entry.Substring($separator + 1)
    }
  }

  $libxmlRoot = Join-Path $InstallRoot ".loom-libxml-$libxmlVersion"
  $libxmlSource = Join-Path $libxmlRoot "source"
  $libxmlBuild = Join-Path $libxmlRoot "build"
  $libxmlInstall = Join-Path $libxmlRoot "install"
  New-Item -ItemType Directory -Force -Path $libxmlSource | Out-Null
  $libxmlUnpackArguments = @(
    "-xzf", $libxmlArchive,
    "-C", $libxmlSource,
    "--strip-components=1"
  )
  & tar.exe @libxmlUnpackArguments
  if ($LASTEXITCODE -ne 0) {
    throw "could not unpack the pinned libxml2 source archive"
  }

  $libxmlOptions = @(
    "-S", $libxmlSource,
    "-B", $libxmlBuild,
    "-G", "Ninja",
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_INSTALL_PREFIX=$libxmlInstall",
    "-DBUILD_SHARED_LIBS=OFF",
    "-DLIBXML2_WITH_C14N=OFF",
    "-DLIBXML2_WITH_CATALOG=OFF",
    "-DLIBXML2_WITH_DEBUG=OFF",
    "-DLIBXML2_WITH_DOCB=OFF",
    "-DLIBXML2_WITH_FTP=OFF",
    "-DLIBXML2_WITH_HTML=OFF",
    "-DLIBXML2_WITH_HTTP=OFF",
    "-DLIBXML2_WITH_ICONV=OFF",
    "-DLIBXML2_WITH_ICU=OFF",
    "-DLIBXML2_WITH_ISO8859X=OFF",
    "-DLIBXML2_WITH_LEGACY=OFF",
    "-DLIBXML2_WITH_LZMA=OFF",
    "-DLIBXML2_WITH_MEM_DEBUG=OFF",
    "-DLIBXML2_WITH_MODULES=OFF",
    "-DLIBXML2_WITH_OUTPUT=ON",
    "-DLIBXML2_WITH_PATTERN=OFF",
    "-DLIBXML2_WITH_PROGRAMS=OFF",
    "-DLIBXML2_WITH_PUSH=OFF",
    "-DLIBXML2_WITH_PYTHON=OFF",
    "-DLIBXML2_WITH_READER=OFF",
    "-DLIBXML2_WITH_REGEXPS=OFF",
    "-DLIBXML2_WITH_RUN_DEBUG=OFF",
    "-DLIBXML2_WITH_SAX1=OFF",
    "-DLIBXML2_WITH_SCHEMAS=OFF",
    "-DLIBXML2_WITH_SCHEMATRON=OFF",
    "-DLIBXML2_WITH_TESTS=OFF",
    "-DLIBXML2_WITH_THREADS=ON",
    "-DLIBXML2_WITH_THREAD_ALLOC=OFF",
    "-DLIBXML2_WITH_TREE=ON",
    "-DLIBXML2_WITH_VALID=OFF",
    "-DLIBXML2_WITH_WRITER=OFF",
    "-DLIBXML2_WITH_XINCLUDE=OFF",
    "-DLIBXML2_WITH_XPATH=OFF",
    "-DLIBXML2_WITH_XPTR=OFF",
    "-DLIBXML2_WITH_ZLIB=OFF",
    "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded"
  )
  & cmake.exe @libxmlOptions
  if ($LASTEXITCODE -ne 0) {
    throw "could not configure LLVM's pinned static libxml2 dependency"
  }
  $buildArguments = @(
    "--build", $libxmlBuild,
    "--target", "install",
    "--config", "Release",
    "--parallel", "2"
  )
  & cmake.exe @buildArguments
  if ($LASTEXITCODE -ne 0) {
    throw "could not build LLVM's pinned static libxml2 dependency"
  }
  $builtLibxml = Join-Path $libxmlInstall "lib\libxml2s.lib"
  if (-not (Test-Path $builtLibxml -PathType Leaf)) {
    throw "the pinned libxml2 build did not install libxml2s.lib"
  }
  Copy-Item -LiteralPath $builtLibxml -Destination $libxmlLibrary
}
if (-not (Test-Path $libxmlLibrary -PathType Leaf) -or (Get-Item $libxmlLibrary).Length -eq 0) {
  throw "LLVM's static system-library closure is incomplete: libxml2s.lib"
}

Add-Content -LiteralPath $EnvironmentFile -Value "LLVM_PATH=$InstallRoot"
Add-Content -LiteralPath $EnvironmentFile -Value "LLVM_SYS_191_PREFIX=$InstallRoot"
Add-Content -LiteralPath $EnvironmentFile -Value "LOOM_CC=$clang"
Add-Content -LiteralPath $PathFile -Value (Join-Path $InstallRoot "bin")
