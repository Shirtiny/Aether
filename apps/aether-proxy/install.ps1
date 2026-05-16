$ErrorActionPreference = 'Stop'

$Repo = if ($env:AETHER_PROXY_RELEASE_REPO) { $env:AETHER_PROXY_RELEASE_REPO } else { 'fawney19/Aether' }
$ReleaseTag = $env:AETHER_PROXY_RELEASE_TAG
$InstallDir = $env:AETHER_PROXY_INSTALL_DIR
$ConfigPath = $env:AETHER_PROXY_CONFIG

function Say([string]$Message) { Write-Host "[Aether Proxy] $Message" }
function Fail([string]$Message) { throw "[Aether Proxy] $Message" }

function Prompt-IfEmpty([string]$Name, [string]$Value, [string]$Prompt) {
  if (-not [string]::IsNullOrWhiteSpace($Value)) { return $Value }
  $Read = Read-Host $Prompt
  if ([string]::IsNullOrWhiteSpace($Read)) { Fail "$Name cannot be empty" }
  return $Read
}

function ConvertTo-TomlQuotedString([string]$Value) {
  return ($Value | ConvertTo-Json -Compress)
}

function Resolve-LatestProxyTag {
  if (-not [string]::IsNullOrWhiteSpace($ReleaseTag)) { return $ReleaseTag }
  $Uri = "https://api.github.com/repos/$Repo/releases?per_page=100"
  $Releases = Invoke-RestMethod -Uri $Uri -Headers @{ 'User-Agent' = 'aether-proxy-installer' }
  $ProxyReleases = @($Releases | Where-Object { -not $_.draft -and $_.tag_name -like 'proxy-v*' } | Sort-Object published_at -Descending)
  if ($ProxyReleases.Count -eq 0) { Fail "No proxy-v* release found in $Repo" }
  return $ProxyReleases[0].tag_name
}

function Test-IsAdministrator {
  $Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
  return $Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Initialize-Paths {
  if ([string]::IsNullOrWhiteSpace($script:InstallDir)) {
    if (Test-IsAdministrator) {
      $script:InstallDir = Join-Path $env:ProgramFiles 'AetherProxy'
    } else {
      $script:InstallDir = Join-Path $env:LOCALAPPDATA 'AetherProxy'
    }
  }
  if ([string]::IsNullOrWhiteSpace($script:ConfigPath)) {
    if (Test-IsAdministrator) {
      $script:ConfigPath = Join-Path $env:ProgramData 'AetherProxy\aether-proxy.toml'
    } else {
      $script:ConfigPath = Join-Path $env:APPDATA 'AetherProxy\aether-proxy.toml'
    }
  }
}

function Install-AetherProxyBinary([string]$Tag, [string]$TempDir) {
  if (-not [Environment]::Is64BitOperatingSystem) { Fail 'Windows release currently supports amd64 only' }
  $Asset = 'aether-proxy-windows-amd64.zip'
  $Base = "https://github.com/$Repo/releases/download/$Tag"
  $Archive = Join-Path $TempDir $Asset
  $Sums = Join-Path $TempDir 'SHA256SUMS.txt'

  Say "Downloading $Tag / $Asset"
  Invoke-WebRequest -Uri "$Base/$Asset" -OutFile $Archive
  try { Invoke-WebRequest -Uri "$Base/SHA256SUMS.txt" -OutFile $Sums } catch { $Sums = $null }

  if ($Sums -and (Test-Path $Sums)) {
    $ExpectedLine = Get-Content $Sums | Where-Object { $_ -match "\s$([regex]::Escape($Asset))$" } | Select-Object -First 1
    if ($ExpectedLine) {
      $Expected = ($ExpectedLine -split '\s+')[0]
      $Actual = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant()
      if ($Actual -ne $Expected.ToLowerInvariant()) { Fail "SHA256 verification failed for $Asset" }
    }
  }

  $ExtractDir = Join-Path $TempDir 'extract'
  Expand-Archive -Path $Archive -DestinationPath $ExtractDir -Force
  $Binary = Join-Path $ExtractDir 'aether-proxy.exe'
  if (-not (Test-Path $Binary)) { Fail 'aether-proxy.exe not found in release asset' }
  New-Item -ItemType Directory -Force -Path $script:InstallDir | Out-Null
  Copy-Item $Binary (Join-Path $script:InstallDir 'aether-proxy.exe') -Force
  Say "Installed binary: $(Join-Path $script:InstallDir 'aether-proxy.exe')"
}

function Test-LegacySingleServerConfig([string]$Path) {
  if (-not (Test-Path $Path)) { return $false }
  foreach ($Line in Get-Content $Path) {
    if ($Line -match '^\s*\[') { return $false }
    if ($Line -match '^\s*(aether_url|management_token)\s*=') { return $true }
  }
  return $false
}

function Test-ServerExists([string]$Path, [string]$QuotedUrl, [string]$QuotedName) {
  if (-not (Test-Path $Path)) { return $false }
  $FoundUrl = $false
  $FoundName = $false
  foreach ($Line in Get-Content $Path) {
    if ($Line -match '^\s*\[\[servers\]\]\s*$') {
      if ($FoundUrl -and $FoundName) { return $true }
      $FoundUrl = $false
      $FoundName = $false
    }
    if ($Line.Trim() -eq "aether_url = $QuotedUrl") { $FoundUrl = $true }
    if ($Line.Trim() -eq "node_name = $QuotedName") { $FoundName = $true }
  }
  return ($FoundUrl -and $FoundName)
}

function Add-ServerConfig([string]$AetherUrl, [string]$ManagementToken, [string]$NodeName) {
  $ConfigDir = Split-Path -Parent $script:ConfigPath
  New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null

  if (Test-LegacySingleServerConfig $script:ConfigPath) {
    Fail "Existing config uses removed top-level aether_url/management_token. Run aether-proxy setup to migrate to [[servers]] first: $script:ConfigPath"
  }

  $QuotedUrl = ConvertTo-TomlQuotedString $AetherUrl
  $QuotedToken = ConvertTo-TomlQuotedString $ManagementToken
  $QuotedName = ConvertTo-TomlQuotedString $NodeName

  if (Test-ServerExists $script:ConfigPath $QuotedUrl $QuotedName) {
    Say "Same aether_url + node_name already exists, skipping config append: $script:ConfigPath"
    return
  }

  if (Test-Path $script:ConfigPath) {
    Copy-Item $script:ConfigPath "$script:ConfigPath.bak.$(Get-Date -Format yyyyMMddHHmmss)" -Force
  }

  $Prefix = if ((Test-Path $script:ConfigPath) -and ((Get-Item $script:ConfigPath).Length -gt 0)) { "`n" } else { '' }
  $Block = @(
    "$Prefix# Added by Aether Proxy one-click installer. Existing config is preserved.",
    '[[servers]]',
    "aether_url = $QuotedUrl",
    "management_token = $QuotedToken",
    "node_name = $QuotedName"
  ) -join "`n"
  Add-Content -Path $script:ConfigPath -Value ($Block + "`n") -Encoding UTF8
  Say "Appended [[servers]] to: $script:ConfigPath"
}

function Main {
  Initialize-Paths
  $AetherUrl = Prompt-IfEmpty 'AETHER_PROXY_AETHER_URL' $env:AETHER_PROXY_AETHER_URL 'Aether URL'
  $ManagementToken = Prompt-IfEmpty 'AETHER_PROXY_MANAGEMENT_TOKEN' $env:AETHER_PROXY_MANAGEMENT_TOKEN 'Management token (ae_xxx)'
  $NodeName = Prompt-IfEmpty 'AETHER_PROXY_NODE_NAME' $env:AETHER_PROXY_NODE_NAME 'Node name'

  $TempDir = Join-Path ([IO.Path]::GetTempPath()) ("aether-proxy-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
  try {
    $Tag = Resolve-LatestProxyTag
    Install-AetherProxyBinary $Tag $TempDir
    Add-ServerConfig $AetherUrl $ManagementToken $NodeName
  } finally {
    Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
  }

  Say 'Complete. Start or configure the node with:'
  Say "  & '$(Join-Path $script:InstallDir 'aether-proxy.exe')' setup '$script:ConfigPath'"
}

Main
