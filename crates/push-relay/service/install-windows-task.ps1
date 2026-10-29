<#
.SYNOPSIS
monokulo-push-relay as a Windows startup task: started at boot as the
LOCAL SERVICE account, and again whenever it stops with an error.

.DESCRIPTION
Run it from an administrator PowerShell in the unzipped folder:

    powershell -ExecutionPolicy Bypass -File service\install-windows-task.ps1

It copies monokulo-push-relay.exe and this script to
C:\Program Files\monokulo\monokulo-push-relay\, keeps the options file,
the secrets, the handles and the log in
C:\ProgramData\monokulo\monokulo-push-relay\ (the options file is the
zip's relay.toml unless one is there already), makes the handle key there
if there is none, registers the task "monokulo-push-relay" and starts it.
Run it again after an upgrade: it keeps the options, secrets and handles.

Each secret is a file in the secrets folder named for its variable, which
only administrators and LOCAL SERVICE can read:
    MONOKULO_RELAY_HANDLE_KEY        made on install; keep a copy
    MONOKULO_RELAY_APNS_KEY          the AuthKey_....p8 file, for APNs
    MONOKULO_RELAY_FCM_CREDENTIALS   the service account JSON, for FCM
Set server.public_uri (and your apps) in relay.toml, then restart it:
    Stop-ScheduledTask monokulo-push-relay; Start-ScheduledTask monokulo-push-relay

The windows service manager isn't used: the relay speaks no service
control protocol (docs/decisions/packaging.md). The task's runner (this
script, -Run) reads the secrets into the relay's environment, starts it,
and starts it again 5 seconds after it stops with an error, saying why in
logs\monokulo-push-relay.log. The status for this machine:
http://127.0.0.1:8446/status. The relay serves no TLS: put an https
reverse proxy in front, serving server.public_uri. Every setting:
https://docs.monokulo.com/push-relay/

.PARAMETER Uninstall
Stop the relay and remove the task and the program; the options, secrets
and handles in C:\ProgramData\monokulo\monokulo-push-relay\ are kept.

.PARAMETER Run
The task's runner; not for running by hand.
#>
param(
    [switch]$Uninstall,
    [switch]$Run
)

$ErrorActionPreference = 'Stop'

# What differs between the relay's script and the Bell's.
$Name = 'monokulo-push-relay'
$OptionsFile = 'relay.toml'
$StoreFlag = '--store-path'
$StoreFile = 'push-relay.db'
$GeneratedSecret = 'MONOKULO_RELAY_HANDLE_KEY'

$Program = Join-Path $env:ProgramFiles "monokulo\$Name"
$Exe = Join-Path $Program "$Name.exe"
$Data = Join-Path $env:ProgramData "monokulo\$Name"
$Options = Join-Path $Data $OptionsFile
$Secrets = Join-Path $Data 'secrets'
$Store = Join-Path $Data "data\$StoreFile"
$Logs = Join-Path $Data 'logs'
$Log = Join-Path $Logs "$Name.log"

# LOCAL SERVICE, SYSTEM and Administrators by SID, whatever the language.
$LocalService = '*S-1-5-19'
$System = '*S-1-5-18'
$Administrators = '*S-1-5-32-544'

# The service as the runner left it, if the task was ended under it.
function Stop-Running {
    Get-Process -Name $Name -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $Exe } |
        Stop-Process -Force
}

if ($Run) {
    Stop-Running
    while ($true) {
        foreach ($secret in Get-ChildItem -LiteralPath $Secrets -File) {
            $value = (Get-Content -LiteralPath $secret.FullName -Raw).TrimEnd("`r", "`n")
            [Environment]::SetEnvironmentVariable($secret.Name, $value, 'Process')
        }
        if (Test-Path -LiteralPath $Log) { Move-Item -LiteralPath $Log "$Log.1" -Force }
        # Quoted one by one: Start-Process joins its arguments with spaces.
        $arguments = @('--options', $Options, $StoreFlag, $Store) | ForEach-Object { '"' + $_ + '"' }
        # The log is its stderr, as it is written; what little goes to stdout
        # is added at the end.
        $process = Start-Process -FilePath $Exe -ArgumentList $arguments -NoNewWindow -PassThru -Wait `
            -RedirectStandardError $Log -RedirectStandardOutput "$Log.out"
        $out = Get-Content -LiteralPath "$Log.out"
        if ($out) { Add-Content -LiteralPath $Log -Value $out }
        Remove-Item -LiteralPath "$Log.out"
        if ($process.ExitCode -eq 0) { exit 0 }
        Start-Sleep -Seconds 5
    }
}

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from an administrator PowerShell.'
}

if ($Uninstall) {
    if (Get-ScheduledTask -TaskName $Name -ErrorAction SilentlyContinue) {
        Stop-ScheduledTask -TaskName $Name
        Unregister-ScheduledTask -TaskName $Name -Confirm:$false
    }
    Stop-Running
    if (Test-Path -LiteralPath $Program) { Remove-Item -LiteralPath $Program -Recurse -Force }
    Write-Output "Removed $Name; its options, secrets and handles are still in $Data."
    exit 0
}

$zip = Split-Path -Parent $PSScriptRoot
if (Get-ScheduledTask -TaskName $Name -ErrorAction SilentlyContinue) {
    Stop-ScheduledTask -TaskName $Name
}
Stop-Running

New-Item -ItemType Directory -Force -Path $Program | Out-Null
Copy-Item -LiteralPath (Join-Path $zip "$Name.exe") -Destination $Exe -Force
Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $Program 'install-windows-task.ps1') -Force

foreach ($dir in @($Data, $Secrets, (Split-Path -Parent $Store), $Logs)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}
# Only administrators and SYSTEM change the folder; LOCAL SERVICE reads it
# all, secrets included, and writes only its handles and its log.
& icacls $Data /inheritance:r /grant:r "${System}:(OI)(CI)F" "${Administrators}:(OI)(CI)F" "${LocalService}:(OI)(CI)RX" | Out-Null
foreach ($dir in @((Split-Path -Parent $Store), $Logs)) {
    & icacls $dir /grant:r "${LocalService}:(OI)(CI)M" | Out-Null
}
if (-not (Test-Path -LiteralPath $Options)) {
    Copy-Item -LiteralPath (Join-Path $zip $OptionsFile) -Destination $Options
}
$generated = Join-Path $Secrets $GeneratedSecret
if (-not (Test-Path -LiteralPath $generated)) {
    $bytes = New-Object byte[] 32
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    Set-Content -LiteralPath $generated -Value (($bytes | ForEach-Object { $_.ToString('x2') }) -join '') -NoNewline
}

$account = (New-Object Security.Principal.SecurityIdentifier 'S-1-5-19').Translate([Security.Principal.NTAccount]).Value
$runner = Join-Path $Program 'install-windows-task.ps1'
$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$runner`" -Run"
$trigger = New-ScheduledTaskTrigger -AtStartup
$taskPrincipal = New-ScheduledTaskPrincipal -UserId $account -LogonType ServiceAccount -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
    -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName $Name -Action $action -Trigger $trigger -Principal $taskPrincipal `
    -Settings $settings -Description "$Name, from $Program" -Force | Out-Null
Start-ScheduledTask -TaskName $Name
Write-Output "Started $Name (task `"$Name`"). Options: $Options; secrets: $Secrets; log: $Log."
