[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$TemporaryRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$probeRoot = Join-Path $TemporaryRoot "loom bootstrap argument probe"
$expected = [ordered]@{
  cacheRoot = Join-Path $probeRoot "download cache"
  installRoot = Join-Path $probeRoot "LLVM install"
  environmentFile = Join-Path $probeRoot "environment output.txt"
  pathFile = Join-Path $probeRoot "path output.txt"
}
$script = Join-Path $PSScriptRoot "bootstrap-windows-llvm.ps1"
$json = & $script `
  -CacheRoot $expected.cacheRoot `
  -InstallRoot $expected.installRoot `
  -EnvironmentFile $expected.environmentFile `
  -PathFile $expected.pathFile `
  -ValidateArgumentsOnly
$actual = $json | ConvertFrom-Json
foreach ($name in $expected.Keys) {
  if ($actual.$name -cne $expected[$name]) {
    throw "Windows LLVM bootstrap changed the $name argument: '$($actual.$name)'"
  }
}
