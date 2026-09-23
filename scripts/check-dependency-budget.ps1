<#
.SYNOPSIS
    Fails the build if Cargo.lock's package count has grown past the
    tracked budget in dependency-budget.txt.

.DESCRIPTION
    This is the CI gate for the zero-dependency rewrite (see
    PLAN-ZERO-DEP.md, S1's "New non-negotiable" and Phase 0 Track A). It
    counts `[[package]]` table occurrences in Cargo.lock, the same way the
    budget file itself was seeded, and compares that count against the
    integer recorded in dependency-budget.txt:

      - current > budget: the dependency count regressed. Fail the build.
      - current < budget: a phase removed dependencies. Don't fail — print
        a note asking whoever is committing to lower dependency-budget.txt
        to match, so the ratchet only ever tightens over time.
      - current == budget: nothing to do.

    No cargo invocation is needed; a text search over Cargo.lock is enough.
#>

[CmdletBinding()]
param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
)

$lockPath = Join-Path $RepoRoot "Cargo.lock"
$budgetPath = Join-Path $RepoRoot "dependency-budget.txt"

if (-not (Test-Path $lockPath)) {
    Write-Host "check-dependency-budget: Cargo.lock not found at '$lockPath'." -ForegroundColor Red
    exit 1
}

if (-not (Test-Path $budgetPath)) {
    Write-Host "check-dependency-budget: dependency-budget.txt not found at '$budgetPath'." -ForegroundColor Red
    exit 1
}

$lockContent = Get-Content -Path $lockPath -Raw
$currentCount = ([regex]::Matches($lockContent, '(?m)^\[\[package\]\]')).Count

$budgetText = (Get-Content -Path $budgetPath -Raw).Trim()
$budgetCount = 0
if (-not [int]::TryParse($budgetText, [ref]$budgetCount)) {
    Write-Host "check-dependency-budget: dependency-budget.txt does not contain a plain integer (found '$budgetText')." -ForegroundColor Red
    exit 1
}

if ($currentCount -gt $budgetCount) {
    Write-Host "check-dependency-budget: dependency count regressed." -ForegroundColor Red
    Write-Host ""
    Write-Host "Cargo.lock now locks $currentCount packages, up from the recorded budget of" -ForegroundColor Red
    Write-Host "$budgetCount in dependency-budget.txt." -ForegroundColor Red
    Write-Host ""
    Write-Host "The zero-dependency rewrite (PLAN-ZERO-DEP.md) only ever ratchets this count" -ForegroundColor Red
    Write-Host "down. If this growth is intentional and justified (see CLAUDE.md's `"no new" -ForegroundColor Red
    Write-Host "dependency without asking`" rule), raise dependency-budget.txt to $currentCount" -ForegroundColor Red
    Write-Host "in the same commit and explain why in CHANGELOG.md. Otherwise, remove the" -ForegroundColor Red
    Write-Host "dependency you added." -ForegroundColor Red
    exit 1
}
elseif ($currentCount -lt $budgetCount) {
    Write-Host "check-dependency-budget: dependency count improved ($currentCount packages, budget is $budgetCount)."
    Write-Host "check-dependency-budget: lower dependency-budget.txt to $currentCount so the ratchet only ever tightens."
}
else {
    Write-Host "dependency budget OK: $currentCount packages"
}

exit 0
