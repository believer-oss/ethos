#Requires -Version 5.1

<#
.SYNOPSIS
    Resets a worktree to the latest main branch for starting fresh work.

.DESCRIPTION
    Parks the current branch locally (never pushed) to free up worktree state,
    then resets the worktree to the latest origin/main. Parking branches exist
    solely to satisfy git worktree branch uniqueness requirements and contain
    only commits already in main.

    Safe to run concurrently from multiple worktrees of the same repo -- git
    operations retry with exponential backoff on lock contention.

.PARAMETER Force
    Skip the uncommitted changes check and reset anyway. Before resetting, ALL
    staged, unstaged, and untracked changes are saved to a git stash so work can
    never be completely lost. The stash ref is printed and can be recovered with
    `git stash apply`.

.EXAMPLE
    .\Reset-WorktreeToMain.ps1
    Resets the worktree to main, aborting if there are uncommitted changes.

.EXAMPLE
    .\Reset-WorktreeToMain.ps1 -Force
    Stashes any staged/uncommitted/untracked changes, then resets to main.
#>

[CmdletBinding()]
param(
    [Parameter()]
    [Alias("f")]
    [switch]$Force
)

$ErrorActionPreference = "Stop"

#region Helper Functions

function Invoke-Git {
    <#
    .SYNOPSIS
        Runs a git command, suppressing stderr from triggering PowerShell errors.
    .DESCRIPTION
        Git writes informational output to stderr (e.g. fetch progress), which
        ErrorActionPreference=Stop treats as terminating errors. This helper
        temporarily sets Continue to capture all output, then checks LASTEXITCODE.
    .OUTPUTS
        Hashtable with Output (string) and ExitCode (int).
    #>
    param([Parameter(Mandatory)][string]$Arguments)

    $prevPref = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $output = Invoke-Expression "git $Arguments 2>&1" | Out-String
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $prevPref

    return @{
        Output   = $output.Trim()
        ExitCode = $exitCode
    }
}

function Invoke-GitWithRetry {
    <#
    .SYNOPSIS
        Runs a git command with automatic retry on lock contention.
    .DESCRIPTION
        Worktrees share a .git directory, so concurrent operations can fail with
        "Unable to create '...lock': File exists". This retries with exponential
        backoff so multiple worktrees can run this script in parallel.
    #>
    param(
        [Parameter(Mandatory)][string]$Arguments,
        [int]$MaxRetries = 5,
        [int]$BaseDelayMs = 500
    )

    for ($attempt = 0; $attempt -le $MaxRetries; $attempt++) {
        $result = Invoke-Git $Arguments
        if ($result.ExitCode -eq 0) {
            return $result
        }

        $isLockError = $result.Output -match "Unable to create.*\.lock" -or
                       $result.Output -match "index\.lock" -or
                       $result.Output -match "Cannot lock ref"

        if (-not $isLockError -or $attempt -eq $MaxRetries) {
            return $result
        }

        $delay = $BaseDelayMs * [Math]::Pow(2, $attempt)
        # Add jitter to avoid thundering herd
        $jitter = Get-Random -Minimum 0 -Maximum ($delay * 0.5)
        $totalDelay = [int]($delay + $jitter)
        Write-Host "  Lock contention detected, retrying in $($totalDelay)ms (attempt $($attempt + 1)/$MaxRetries)..." -ForegroundColor Yellow
        Start-Sleep -Milliseconds $totalDelay
    }
}

function Get-RepoRoot {
    $result = Invoke-Git "rev-parse --show-toplevel"
    if ($result.ExitCode -ne 0) {
        return $null
    }
    return $result.Output
}

function Get-GitUsername {
    <#
    .SYNOPSIS
        Returns the username portion of the configured git email.
    #>
    $result = Invoke-Git "config user.email"
    if ($result.ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($result.Output)) {
        throw "Git user.email is not configured. Run: git config user.email <your-email>"
    }
    if ($result.Output -match "^([^@]+)@") {
        return $Matches[1].ToLower()
    }
    throw "Could not parse a username from git user.email: $($result.Output)"
}

function Test-UncommittedChanges {
    $result = Invoke-Git "status --porcelain"
    if ($result.ExitCode -ne 0) {
        throw "Failed to check git status: $($result.Output)"
    }
    return -not [string]::IsNullOrWhiteSpace($result.Output)
}

#endregion

#region Main Script

# Step 1: Pre-flight checks
$repoRoot = Get-RepoRoot
if (-not $repoRoot) {
    Write-Error "Not a git repository. Please run from within a git repository."
    exit 1
}

Push-Location $repoRoot
try {
    $gitUser = Get-GitUsername
    # Worktree name is the leaf of the worktree root, so parking branches from
    # different worktrees never collide (e.g. ethos, alpha, bravo).
    $worktreeName = (Split-Path -Leaf $repoRoot).ToLower()

    $result = Invoke-Git "rev-parse --abbrev-ref HEAD"
    if ($result.ExitCode -ne 0) {
        Write-Error "Failed to determine current branch: $($result.Output)"
        exit 1
    }
    $currentBranch = $result.Output

    Write-Host "Current branch: $currentBranch" -ForegroundColor Gray
    Write-Host "Git user: $gitUser" -ForegroundColor Gray
    Write-Host "Worktree: $worktreeName ($repoRoot)" -ForegroundColor Gray
    Write-Host ""

    $stashRef = $null
    if (Test-UncommittedChanges) {
        if ($Force) {
            # ALWAYS stash before a forced reset so working-tree work can never be
            # completely lost. --include-untracked captures staged, unstaged, AND
            # untracked files. Worktrees share the .git dir, so the stash is
            # recoverable from any worktree via `git stash list` / `git stash apply`.
            $stashMessage = "Reset-WorktreeToMain auto-stash: $worktreeName ($currentBranch) $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
            Write-Host "Stashing staged/uncommitted/untracked changes before reset (-Force)..." -ForegroundColor Cyan
            $stashResult = Invoke-GitWithRetry "stash push --include-untracked --message `"$stashMessage`""
            if ($stashResult.ExitCode -ne 0) {
                Write-Error "Failed to stash uncommitted changes; aborting to avoid losing work:`n$($stashResult.Output)"
                exit 1
            }
            # Capture the stash ref (newest entry) so we can report it for recovery.
            $stashListResult = Invoke-Git "stash list --format=`"%gd: %gs`" -1"
            $stashRef = if ($stashListResult.ExitCode -eq 0) { $stashListResult.Output } else { "stash@{0}" }
            Write-Host "  Saved to: $stashRef" -ForegroundColor Green
        } else {
            Write-Error "Uncommitted changes detected. Commit or stash changes before resetting, or use -Force to stash and discard."
            exit 1
        }
    }

    # Check for unpushed commits not in main that would be lost
    $result = Invoke-Git "log origin/main..HEAD --oneline"
    if ($result.ExitCode -ne 0) {
        # origin/main may not exist yet; fetch first and recheck after
        Write-Host "Could not compare with origin/main, will check after fetch." -ForegroundColor Yellow
    } elseif (-not [string]::IsNullOrWhiteSpace($result.Output)) {
        # There are commits not in main -- check if they've been pushed to the remote tracking branch
        $trackingResult = Invoke-Git "rev-parse --abbrev-ref --symbolic-full-name `"@{upstream}`""
        if ($trackingResult.ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($trackingResult.Output)) {
            # No upstream configured -- all commits are unpushed
            $unpushedOutput = $result.Output
        } else {
            $upstream = $trackingResult.Output
            $unpushedResult = Invoke-Git "log $upstream..HEAD --oneline"
            $unpushedOutput = if ($unpushedResult.ExitCode -eq 0) { $unpushedResult.Output } else { $result.Output }
        }

        if (-not [string]::IsNullOrWhiteSpace($unpushedOutput)) {
            # Check if these commits were already merged to main (e.g., squash-merged via PR
            # and the remote branch deleted). The commits appear "unpushed" because the local
            # SHA differs from the squash-merge commit on main, but the content is already there.
            $alreadyMerged = $false
            $lsRemoteResult = Invoke-Git "ls-remote --heads origin `"$currentBranch`""
            $remoteBranchExists = $lsRemoteResult.ExitCode -eq 0 -and -not [string]::IsNullOrWhiteSpace($lsRemoteResult.Output)

            if (-not $remoteBranchExists) {
                # Remote branch is gone -- use git cherry to check if patch content is in main.
                # git cherry compares patch-ids (diff content), so it detects squash-merged
                # commits even though the SHAs differ.
                $cherryResult = Invoke-Git "cherry origin/main HEAD"
                if ($cherryResult.ExitCode -eq 0 -and -not [string]::IsNullOrWhiteSpace($cherryResult.Output)) {
                    $unmergedCommits = ($cherryResult.Output -split "`n") | Where-Object { $_.TrimStart().StartsWith("+") }
                    if (-not $unmergedCommits) {
                        $alreadyMerged = $true
                    }
                }
            }

            if ($alreadyMerged) {
                Write-Host "Local commits already merged to main (remote branch deleted). Safe to reset." -ForegroundColor Green
            } else {
                $commitCount = ($unpushedOutput -split "`n").Count
                if ($Force) {
                    Write-Host "WARNING: $commitCount unpushed commit(s) will be discarded (-Force):" -ForegroundColor Yellow
                    Write-Host $unpushedOutput -ForegroundColor Yellow
                } else {
                    Write-Error "Branch has $commitCount unpushed commit(s) that would be lost:`n$unpushedOutput`nPush the branch first to preserve the work, or use -Force to discard."
                    exit 1
                }
            }
        }
    }

    # Step 2: Fetch latest main
    Write-Host "Fetching latest main from origin..." -ForegroundColor Cyan
    $result = Invoke-GitWithRetry "fetch origin main"
    if ($result.ExitCode -ne 0) {
        Write-Error "Failed to fetch origin/main. You may be offline or the remote is unavailable.`n$($result.Output)"
        exit 1
    }

    # Step 3: Create parking branch at origin/main and switch to it (single atomic operation).
    # Using `switch -C` instead of separate `checkout -B` + `reset --hard` to minimize
    # lock contention when multiple worktrees run this script concurrently.
    $dateStamp = Get-Date -Format "yyyyMMdd"
    $parkingBranch = "llm/$gitUser/$worktreeName-parking-$dateStamp"

    Write-Host "Switching to parking branch at origin/main: $parkingBranch" -ForegroundColor Cyan
    $switchArgs = "switch -C `"$parkingBranch`" origin/main --discard-changes"
    $result = Invoke-GitWithRetry $switchArgs
    if ($result.ExitCode -ne 0) {
        Write-Error "Failed to switch to parking branch at origin/main: $($result.Output)"
        exit 1
    }

    # Step 4: Rebuild untracked cache.
    # The untracked cache (core.untrackedCache) can go stale after branch switches,
    # causing ghost untracked files and "could not open directory" warnings.
    Write-Host "Rebuilding untracked cache..." -ForegroundColor Cyan
    $result = Invoke-Git "update-index --force-untracked-cache"
    if ($result.ExitCode -ne 0) {
        Write-Host "WARNING: Failed to rebuild untracked cache: $($result.Output)" -ForegroundColor Yellow
    }

    # Step 5: Verify state
    $result = Invoke-Git "log -1 --oneline"
    if ($result.ExitCode -ne 0) {
        Write-Error "Failed to verify HEAD commit: $($result.Output)"
        exit 1
    }
    $headCommit = $result.Output

    if (Test-UncommittedChanges) {
        Write-Host "WARNING: Unexpected uncommitted changes after reset." -ForegroundColor Yellow
    }

    # Step 6: Output results
    Write-Host ""
    Write-Host "Worktree reset to main" -ForegroundColor Green
    Write-Host "  Parking branch: $parkingBranch (local only, do not push)" -ForegroundColor Gray
    Write-Host "  Now at: $headCommit" -ForegroundColor Gray
    if ($stashRef) {
        Write-Host "  Stashed work: $stashRef" -ForegroundColor Gray
        Write-Host "  Recover with: git stash apply `"$($stashRef.Split(':')[0])`"" -ForegroundColor Gray
    }
    Write-Host "  Ready for new work" -ForegroundColor Gray
    Write-Host ""
    Write-Host "  Note: switching branches can invalidate build state. If the frontend" -ForegroundColor DarkGray
    Write-Host "  was built from another branch, re-run 'yarn' / 'yarn package' as needed." -ForegroundColor DarkGray

    exit 0
} finally {
    Pop-Location
}

#endregion
