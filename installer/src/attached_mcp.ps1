$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Find-Listener([int]$Port) {
    $rows = @(& $script:Netstat -ano)
    if ($LASTEXITCODE -ne 0) { throw 'Listener inspection failed.' }
    foreach ($line in $rows) {
        # Listening TCP sockets have a zero remote port in numeric netstat output.
        if ($line -match '^\s*TCP\s+(\S+):([0-9]+)\s+(?:0\.0\.0\.0|\[::\]):0\s+\S+\s+([0-9]+)\s*$' -and
            [int]$Matches[2] -eq $Port) {
            [pscustomobject]@{ Address = $Matches[1]; ProcessId = [int]$Matches[3] }
        }
    }
}

function Get-Identity([int]$ProcessId) {
    Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $ProcessId" -OperationTimeoutSec 3
}

function Test-Same($Left, $Right) {
    $null -ne $Left -and $null -ne $Right -and $Left.ProcessId -eq $Right.ProcessId -and
        $null -ne $Left.CreationDate -and $Left.CreationDate -eq $Right.CreationDate
}

function Test-ChildOf($Child, $Parent) {
    $null -ne $Child -and $null -ne $Parent -and $Child.ProcessId -ne $Parent.ProcessId -and
        $Child.ParentProcessId -eq $Parent.ProcessId -and $null -ne $Child.CreationDate -and
        $null -ne $Parent.CreationDate -and $Child.CreationDate -ge $Parent.CreationDate
}

function Test-ServerPython($Process, [string]$Server) {
    if ($null -eq $Process -or $Process.Name -ne 'python.exe' -or
        $Process.CommandLine -notmatch '(?i)(?:^|\s)-I\s+(.+?)\s*$') {
        return $false
    }
    $scriptPath = $Matches[1].Trim('"')
    $scriptPath -eq $Server -or $scriptPath -eq 'comfy_mcp_server.py'
}

function Test-Launcher($Process, [string]$Launcher, [string]$Cmd) {
    if ($null -eq $Process -or $Process.ExecutablePath -ne $Cmd -or
        $Process.CommandLine -notmatch '(?i)(?:^|\s)/c\s+(.+?)\s*$') {
        return $false
    }
    $invocation = $Matches[1].Trim('"')
    $invocation -eq $Launcher -or $invocation -eq 'start_mcp_server.bat'
}

function Select-Owned($Rows, [string]$Launcher, [string]$Shim, [string]$Server, [string]$Cmd, [scriptblock]$Lookup) {
    $owners = @($Rows | Select-Object -ExpandProperty ProcessId -Unique)
    if ($owners.Count -ne 1 -or @($Rows | Where-Object Address -eq '127.0.0.1').Count -eq 0) {
        return
    }
    $process = & $Lookup $owners[0]
    if (-not (Test-ServerPython $process $Server)) { return }
    $parent = & $Lookup $process.ParentProcessId
    if (-not (Test-ChildOf $process $parent)) { return }
    if ($parent.ExecutablePath -eq $Shim -and (Test-ServerPython $parent $Server)) {
        $wrapper = & $Lookup $parent.ParentProcessId
        $batchChild = $parent
    } elseif ($process.ExecutablePath -eq $Shim) {
        $wrapper = $parent
        $batchChild = $process
    } else {
        return
    }
    # Sign-in names the full batch path. Hidden app launches use the fixed name
    # and its private venv's absolute path establishes the directory identity.
    if (-not (Test-ChildOf $batchChild $wrapper) -or -not (Test-Launcher $wrapper $Launcher $Cmd)) {
        return
    }
    [pscustomobject]@{ Listener = $process; Wrapper = $wrapper }
}

function Get-Children([int]$ProcessId) {
    Get-CimInstance -ClassName Win32_Process -Filter "ParentProcessId = $ProcessId" -OperationTimeoutSec 3
}

function Select-LaunchedPython($Rows, $Parent, $ParentExit, [string]$Server, [string]$Image = '') {
    foreach ($child in $Rows) {
        if ((Test-ChildOf $child $Parent) -and $child.CreationDate -le $ParentExit -and
            (Test-ServerPython $child $Server) -and (!$Image -or $child.ExecutablePath -eq $Image)) {
            $child
        }
    }
}

function Stop-Identity($Identity, [switch]$ReportExit) {
    $current = Get-Identity $Identity.ProcessId
    if ($null -eq $current) { return }
    if (-not (Test-Same $Identity $current)) { throw 'Process identity changed.' }
    $target = Get-Process -Id $Identity.ProcessId -ErrorAction SilentlyContinue
    if ($null -eq $target) { return }
    try {
        $null = $target.SafeHandle
        if (-not (Test-Same $Identity (Get-Identity $Identity.ProcessId))) {
            throw 'Process identity changed while acquiring the handle.'
        }
        Stop-Process -InputObject $target -Force
        if (-not $target.WaitForExit(6000)) { throw 'The verified listener did not exit.' }
        if ($ReportExit) { $target.ExitTime }
    } finally {
        $target.Dispose()
    }
}

function Stop-FailedStart($Wrapper, [string]$Shim, [string]$Server) {
    # Stop the batch before inspecting its children so it cannot launch another.
    # Bound the lookup by the recorded exit time in case Windows reuses a PID.
    $wrapperExit = Stop-Identity $Wrapper -ReportExit
    if ($null -eq $wrapperExit) { return }
    $children = @(Get-Children $Wrapper.ProcessId)
    foreach ($shimProcess in @(Select-LaunchedPython $children $Wrapper $wrapperExit $Server $Shim)) {
        $shimExit = Stop-Identity $shimProcess -ReportExit
        if ($null -eq $shimExit) { continue }
        $children = @(Get-Children $shimProcess.ProcessId)
        foreach ($worker in @(Select-LaunchedPython $children $shimProcess $shimExit $Server)) {
            Stop-Identity $worker
        }
    }
}

function Is-Regular([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force
    -not $item.PSIsContainer -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
}

try {
    $port = [int]$env:RB_ATTACHED_PORT
    $launcher = $env:RB_ATTACHED_LAUNCHER
    $operation = $env:RB_ATTACHED_OPERATION
    if ($port -lt 1 -or $port -gt 65535 -or $operation -notin @('start', 'stop') -or
        -not [IO.Path]::IsPathRooted($launcher) -or [IO.Path]::GetFileName($launcher) -ne 'start_mcp_server.bat') {
        exit 1
    }
    $root = [IO.Path]::GetDirectoryName($launcher)
    $shim = Join-Path $root '.venv\Scripts\python.exe'
    $server = Join-Path $root 'comfy_mcp_server.py'
    $cmd = Join-Path $env:SystemRoot 'System32\cmd.exe'
    $script:Netstat = Join-Path $env:SystemRoot 'System32\netstat.exe'
    if (-not (Is-Regular $launcher) -or -not (Is-Regular $shim) -or -not (Is-Regular $server)) {
        exit 1
    }

    $lookup = { param($ProcessId) Get-Identity $ProcessId }
    $rows = @(Find-Listener $port)
    if ($rows.Count -ne 0) {
        $owned = Select-Owned $rows $launcher $shim $server $cmd $lookup
        if ($null -eq $owned) { exit 2 }
        if ($operation -eq 'start') { exit 0 }

        $fresh = Select-Owned @(Find-Listener $port) $launcher $shim $server $cmd $lookup
        if ($null -eq $fresh -or -not (Test-Same $owned.Listener $fresh.Listener) -or
            -not (Test-Same $owned.Wrapper $fresh.Wrapper)) { exit 2 }
        try {
            Stop-Identity $fresh.Listener
        } catch {
            exit 4
        }
        # The batch may be paused after Python exits. Never stop its process tree.
        try { Stop-Identity $fresh.Wrapper } catch {}
        if (@(Find-Listener $port).Count -ne 0) { exit 4 }
        exit 0
    }

    if ($operation -eq 'stop') { exit 0 }
    $env:MCP_PORT = [string]$port
    $started = Start-Process -FilePath $cmd -WorkingDirectory $root -ArgumentList '/d /c start_mcp_server.bat' -WindowStyle Hidden -PassThru
    $wrapper = $null
    $ready = $false
    try {
        $null = $started.SafeHandle
        $startedAt = $started.StartTime
        $candidate = Get-Identity $started.Id
        if (-not (Test-Launcher $candidate $launcher $cmd) -or $null -eq $candidate.CreationDate -or
            [Math]::Abs(($candidate.CreationDate - $startedAt).TotalMilliseconds) -gt 1.0) {
            exit 3
        }
        $wrapper = $candidate
        $until = [DateTime]::UtcNow.AddSeconds(15)
        do {
            $rows = @(Find-Listener $port)
            if ($rows.Count -ne 0) {
                $owned = Select-Owned $rows $launcher $shim $server $cmd $lookup
                if ($null -ne $owned -and (Test-Same $owned.Wrapper $wrapper)) {
                    $ready = $true
                    exit 0
                }
                exit 2
            }
            if ($started.HasExited) { break }
            Start-Sleep -Milliseconds 350
        } while ([DateTime]::UtcNow -lt $until)
        exit 3
    } finally {
        try {
            if (!$ready -and $null -ne $wrapper) { Stop-FailedStart $wrapper $shim $server }
        } catch {
            # An unverified process must stay untouched.
        } finally {
            $started.Dispose()
        }
    }
} catch {
    exit 1
}
