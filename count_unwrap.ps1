$ErrorActionPreference = 'Stop'
$projectRoot = $PSScriptRoot

$results = @{}
$crateTotals = @{}

Get-ChildItem -Path $projectRoot -Filter '*.rs' -Recurse |
    Where-Object { $_.FullName -notmatch '\\target\\' } |
    ForEach-Object {
        $fileName = $_.Name
        # Files entirely in test: *_tests.rs, tests.rs, test_*.rs, or inside tests/ dir
        $isTestFile = ($fileName -match '_tests\.rs$' -or $fileName -eq 'tests.rs' -or $fileName -match '^test_') -or ($_.FullName -match '\\tests\\')
        
        if ($isTestFile) {
            $count = (Select-String -Path $_.FullName -Pattern 'unwrap\(\)' -AllMatches).Matches.Count
            if ($count -gt 0) {
                $relPath = $_.FullName.Substring($projectRoot.Length)
                if ($relPath -match '\\crates\\([^\\]+)') { $crate = $matches[1] } else { $crate = 'root' }
                $results[$relPath] = @{ prod = 0; test = $count; crate = $crate; isTestFile = $true }
                if (-not $crateTotals.ContainsKey($crate)) { $crateTotals[$crate] = @{ prod = 0; test = 0 } }
                $crateTotals[$crate].test += $count
            }
            return
        }

        $lines = Get-Content $_.FullName -ReadCount 0
        $inTest = $false
        $prodCount = 0
        $testCount = 0
        for ($i = 0; $i -lt $lines.Count; $i++) {
            $line = $lines[$i]
            if ($line -match '#\[cfg\(test\)\]' -or $line -match '^mod tests \{') {
                $inTest = $true
            }
            if ($line -match 'unwrap\(\)') {
                if ($inTest) { $testCount++ } else { $prodCount++ }
            }
        }

        if ($prodCount -gt 0 -or $testCount -gt 0) {
            $relPath = $_.FullName.Substring($projectRoot.Length)
            if ($relPath -match '\\crates\\([^\\]+)') { $crate = $matches[1] } else { $crate = 'root' }
            $results[$relPath] = @{ prod = $prodCount; test = $testCount; crate = $crate; isTestFile = $false }
            if (-not $crateTotals.ContainsKey($crate)) { $crateTotals[$crate] = @{ prod = 0; test = 0 } }
            $crateTotals[$crate].prod += $prodCount
            $crateTotals[$crate].test += $testCount
        }
    }

Write-Output "=== CRATE TOTALS ==="
foreach ($crate in ($crateTotals.Keys | Sort-Object)) {
    $t = $crateTotals[$crate]
    $total = $t.prod + $t.test
    Write-Output "$crate | prod=$($t.prod) | test=$($t.test) | total=$total"
}

Write-Output ""
Write-Output "=== TOP 15 PROD UNWRAP (production code only) ==="
$results.GetEnumerator() |
    Where-Object { $_.Value.prod -gt 0 } |
    Sort-Object { -$_.Value.prod } |
    Select-Object -First 15 |
    ForEach-Object {
        $tag = if ($_.Value.isTestFile) { "[TEST_FILE]" } else { "" }
        Write-Output "$($_.Value.crate) | prod=$($_.Value.prod) | test=$($_.Value.test) $tag | $($_.Key)"
    }

$grandProd = 0; $grandTest = 0
foreach ($c in $crateTotals.Values) { $grandProd += $c.prod; $grandTest += $c.test }
$grandTotal = $grandProd + $grandTest
Write-Output ""
Write-Output "=== GRAND TOTAL ==="
Write-Output "Production code: $grandProd"
Write-Output "Test code: $grandTest"
Write-Output "Total: $grandTotal"
Write-Output "Production %: $([math]::Round($grandProd / $grandTotal * 100, 1))%"
Write-Output "Test %: $([math]::Round($grandTest / $grandTotal * 100, 1))%"
