# Install Sterna and the inference gateway on Windows from a GitHub release.
#
#   irm https://harzerheribert.github.io/sterna/install.ps1 | iex
#   $env:STERNA_VERSION = 'v0.1.0-pre.18'; irm ... | iex    # a specific release
#   $env:STERNA_DESKTOP = '1'; irm ... | iex                # the desktop app too
#
# The Windows twin of install.sh, and nothing more:
#   1. picks the release (the newest, pre-releases included, or
#      $env:STERNA_VERSION) and this machine's zip;
#   2. refuses the zip unless its SHA-256 matches the release's SHA256SUMS;
#   3. unpacks it into %LOCALAPPDATA%\Programs\sterna\versions\<tag>\bin -- a
#      fresh directory, never over a binary that may be running -- and points
#      the `current` junction at it;
#   4. adds ...\sterna\current\bin to your user PATH (no administrator
#      rights, no system PATH);
#   5. downloads the CLIProxyAPI build the release pins (cliproxyapi.toml),
#      refuses it unless its SHA-256 matches the pin, and hands it to
#      `inference-gateway subscriptions adopt-binary`;
#   6. with $env:STERNA_DESKTOP set, the desktop app from the same release,
#      verified the same way, into ...\versions\<tag>\desktop, and a Start
#      menu shortcut to ...\sterna\current\desktop\Sterna.exe.
#      Sterna's own updates keep it in step from then on.
# It installs no harness and touches no credential. It runs in Windows
# PowerShell 5.1 as well as PowerShell 7.

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 draws a progress bar per downloaded chunk, which
# makes a download many times slower.
$ProgressPreference = 'SilentlyContinue'
if ($PSVersionTable.PSVersion.Major -lt 6) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
}

function Get-Setting([string]$Name, [string]$Default) {
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrEmpty($value)) { return $Default }
    return $value
}

$OnWindows = [Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT
$Repo = Get-Setting 'STERNA_REPO' 'HarzerHeribert/sterna'
# Test seams: where releases are listed and downloaded from, where the install
# goes, which archive to take and whether to touch PATH -- exactly as
# install.sh's STERNA_* variables are.
$Api = Get-Setting 'STERNA_RELEASES_API' "https://api.github.com/repos/$Repo/releases?per_page=30"
$Downloads = Get-Setting 'STERNA_RELEASE_DOWNLOADS' "https://github.com/$Repo/releases/download"
$BrokerDownloads = Get-Setting 'STERNA_BROKER_DOWNLOADS' ''
$DefaultRoot = ''
if ($env:LOCALAPPDATA) { $DefaultRoot = Join-Path $env:LOCALAPPDATA 'Programs\sterna' }
$Root = Get-Setting 'STERNA_HOME' $DefaultRoot
$PathScope = Get-Setting 'STERNA_PATH_SCOPE' 'User'
$Desktop = Get-Setting 'STERNA_DESKTOP' ''

function Say([string]$Text) { Write-Host $Text }
function Fail([string]$Text) { throw "install.ps1: $Text" }

function Fetch([string]$Url, [string]$Dest) {
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
}

function Sha256Of([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# The SHA-256 the release's SHA256SUMS lists for $Name, or '' when it lists
# none.
function Get-ListedSum([string]$Sums, [string]$Name) {
    $listed = ''
    foreach ($line in Get-Content -LiteralPath $Sums) {
        $fields = $line -split '\s+', 2
        if ($fields.Count -eq 2 -and $fields[1].TrimStart('*') -eq $Name) { $listed = $fields[0].ToLowerInvariant() }
    }
    return $listed
}

# `current` points at the version in use. A junction needs no administrator
# rights, and Sterna's updater reads it like any other link.
function Set-DirLink([string]$Link, [string]$Target) {
    if (Test-Path -LiteralPath $Link) {
        # Removes the link itself; never follows it into the version it
        # points at.
        [IO.Directory]::Delete($Link, $false)
    }
    if ($OnWindows) {
        New-Item -ItemType Junction -Path $Link -Target $Target | Out-Null
    } else {
        New-Item -ItemType SymbolicLink -Path $Link -Target $Target | Out-Null
    }
}

if (-not $Root) { Fail 'LOCALAPPDATA is not set; set STERNA_HOME to the folder to install into' }

$Target = Get-Setting 'STERNA_TARGET' ''
if (-not $Target) {
    if (-not $OnWindows) {
        Fail 'this installer is for Windows; on macOS and Linux run: curl -fsSL https://harzerheribert.github.io/sterna/install.sh | sh'
    }
    # PROCESSOR_ARCHITEW6432 is set when a 32-bit PowerShell runs on 64-bit
    # Windows; it names the machine, PROCESSOR_ARCHITECTURE the process.
    $Arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $Arch) { $Arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($Arch) {
        'AMD64' { $Target = 'x86_64-pc-windows-msvc' }
        'ARM64' { $Target = 'aarch64-pc-windows-msvc' }
        default { Fail "this installer does not support $Arch; release archives are at https://github.com/$Repo/releases" }
    }
}

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("sterna-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    $Tag = Get-Setting 'STERNA_VERSION' ''
    if (-not $Tag) {
        $releases = Invoke-RestMethod -Uri $Api -UseBasicParsing
        # The list's own order puts pre.9 above pre.10: rank the tags by
        # version, a release above its own pre-releases.
        $ranked = foreach ($release in $releases) {
            $name = [string]$release.tag_name
            if ($name -match '^v(\d+)\.(\d+)\.(\d+)(?:-pre\.(\d+))?$') {
                $final = 0; $pre = 0
                if ($Matches[4]) { $pre = [long]$Matches[4] } else { $final = 1 }
                [pscustomobject]@{
                    Tag = $name; Major = [long]$Matches[1]; Minor = [long]$Matches[2]
                    Patch = [long]$Matches[3]; Final = $final; Pre = $pre
                }
            }
        }
        $newest = $ranked | Sort-Object Major, Minor, Patch, Final, Pre | Select-Object -Last 1
        if (-not $newest) { Fail "could not read the newest release of $Repo" }
        $Tag = $newest.Tag
    }
    $Version = $Tag.Substring(1)
    $Archive = "sterna-$Version-$Target.zip"
    $Base = "$Downloads/$Tag"

    Say "Installing $Tag for $Target"
    Fetch "$Base/$Archive" (Join-Path $Tmp $Archive)
    Fetch "$Base/SHA256SUMS" (Join-Path $Tmp 'SHA256SUMS')
    $want = Get-ListedSum (Join-Path $Tmp 'SHA256SUMS') $Archive
    if (-not $want) { Fail "$Archive is not listed in the release's SHA256SUMS" }
    if ((Sha256Of (Join-Path $Tmp $Archive)) -ne $want) { Fail "$Archive does not match its SHA-256; refusing it" }

    $Versions = Join-Path $Root 'versions'
    $Dest = Join-Path $Versions $Tag
    $Sterna = Join-Path $Dest 'bin\sterna.exe'
    if (Test-Path -LiteralPath $Sterna) {
        Say "$Tag is already installed at $Dest"
    } else {
        Expand-Archive -LiteralPath (Join-Path $Tmp $Archive) -DestinationPath $Tmp -Force
        $stage = Join-Path $Tmp "sterna-$Version-$Target"
        $partial = "$Dest.partial"
        if (Test-Path -LiteralPath $partial) { Remove-Item -LiteralPath $partial -Recurse -Force }
        New-Item -ItemType Directory -Path (Join-Path $partial 'bin') | Out-Null
        foreach ($b in 'sterna.exe', 'inference-gateway.exe') {
            $from = Join-Path $stage $b
            if (Test-Path -LiteralPath $from) { Copy-Item -LiteralPath $from -Destination (Join-Path $partial 'bin') }
        }
        $pinFrom = Join-Path $stage 'cliproxyapi.toml'
        if (Test-Path -LiteralPath $pinFrom) { Copy-Item -LiteralPath $pinFrom -Destination $partial }
        if (-not (Test-Path -LiteralPath (Join-Path $partial 'bin\sterna.exe'))) { Fail 'the archive carried no sterna binary' }
        Move-Item -LiteralPath $partial -Destination $Dest
    }
    Set-DirLink (Join-Path $Root 'current') $Dest

    # The subscription broker the release was built with.
    $pin = Join-Path $Dest 'cliproxyapi.toml'
    if (Test-Path -LiteralPath $pin) {
        $text = Get-Content -LiteralPath $pin -Raw
        $brokerRepo = ''; $brokerVersion = ''; $name = ''; $sum = ''
        if ($text -match '(?m)^repository = "([^"]*)"') { $brokerRepo = $Matches[1] }
        if ($text -match '(?m)^version = "([^"]*)"') { $brokerVersion = $Matches[1] }
        $entry = [regex]::Match($text, '(?m)^' + [regex]::Escape($Target) + ' = \{[^}]*\}')
        if ($entry.Success) {
            if ($entry.Value -match 'name = "([^"]*)"') { $name = $Matches[1] }
            if ($entry.Value -match 'sha256 = "([^"]*)"') { $sum = $Matches[1] }
        }
        $stamp = Join-Path $Root 'broker-version'
        $adopted = ''
        if (Test-Path -LiteralPath $stamp) { $adopted = (Get-Content -LiteralPath $stamp -Raw).Trim() }
        if ($name -and $sum -and $adopted -ne $brokerVersion) {
            $from = $BrokerDownloads
            if (-not $from) { $from = "https://github.com/$brokerRepo/releases/download" }
            Fetch "$from/v$brokerVersion/$name" (Join-Path $Tmp $name)
            if ((Sha256Of (Join-Path $Tmp $name)) -ne $sum) { Fail "$name does not match the SHA-256 the release pins; refusing it" }
            $brokerDir = Join-Path $Tmp 'broker'
            Expand-Archive -LiteralPath (Join-Path $Tmp $name) -DestinationPath $brokerDir -Force
            $gateway = Join-Path $Dest 'bin\inference-gateway.exe'
            & $gateway subscriptions adopt-binary (Join-Path $brokerDir 'cli-proxy-api.exe') | Out-Null
            if ($LASTEXITCODE -ne 0) { Fail "inference-gateway refused to adopt CLIProxyAPI $brokerVersion" }
            Set-Content -LiteralPath $stamp -Value $brokerVersion -NoNewline
            Say "Subscription broker: CLIProxyAPI $brokerVersion"
        }
    }

    # The desktop app, from the same release.
    if ($Desktop) {
        $appArchive = "sterna-desktop-$Version-$Target.zip"
        $appWant = Get-ListedSum (Join-Path $Tmp 'SHA256SUMS') $appArchive
        if (-not $appWant) { Fail "$Tag carries no desktop app for $Target; sterna itself is installed" }
        $appDir = Join-Path $Dest 'desktop'
        if (-not (Test-Path -LiteralPath (Join-Path $appDir 'Sterna.exe'))) {
            Fetch "$Base/$appArchive" (Join-Path $Tmp $appArchive)
            if ((Sha256Of (Join-Path $Tmp $appArchive)) -ne $appWant) { Fail "$appArchive does not match its SHA-256; refusing it" }
            $appStage = Join-Path $Tmp 'desktop-stage'
            Expand-Archive -LiteralPath (Join-Path $Tmp $appArchive) -DestinationPath $appStage -Force
            if (-not (Test-Path -LiteralPath (Join-Path $appStage 'desktop\Sterna.exe'))) { Fail "$appArchive carried no Sterna.exe" }
            Move-Item -LiteralPath (Join-Path $appStage 'desktop') -Destination $appDir
        }
        Set-Content -LiteralPath (Join-Path $Root 'desktop') -Value '' -NoNewline
        $app = Join-Path $Root 'current\desktop\Sterna.exe'
        if ($OnWindows) {
            $menu = Get-Setting 'STERNA_START_MENU' ([Environment]::GetFolderPath('Programs'))
            $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $menu 'Sterna.lnk'))
            $shortcut.TargetPath = $app
            $shortcut.WorkingDirectory = Join-Path $Root 'current\desktop'
            $shortcut.Description = 'Watch and answer your coding sessions'
            $shortcut.Save()
            Say "Desktop app: Sterna in your Start menu ($app)"
        } else {
            Say "Desktop app: $app"
        }
    }

    $BinDir = Join-Path $Root 'current\bin'
    Say "Installed $Tag. Sterna updates itself from here on; run ``sterna`` to start, ``sterna doctor`` to check."
    if ($PathScope -eq 'None') {
        Say "Add $BinDir to your PATH to run sterna from any shell."
    } else {
        $userPath = [Environment]::GetEnvironmentVariable('Path', $PathScope)
        $entries = @()
        if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ } }
        if ($entries -notcontains $BinDir) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $BinDir) -join ';'), $PathScope)
            Say "Added $BinDir to your user PATH. Open a new terminal to run sterna."
        }
        # This window too, so `sterna` works right away.
        if (($env:Path -split ';') -notcontains $BinDir) { $env:Path = "$env:Path;$BinDir" }
    }
} finally {
    Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
