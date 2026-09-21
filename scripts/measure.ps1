# Indulási idő és memória mérése.
# Használat: .\scripts\measure.ps1 -Image C:\kepek\foto.jpg [-Runs 5] [-Exe target\release\fastpic.exe]
#
# A program `--bench <fájl>` kapcsolóval a process létrehozásától az első, képet tartalmazó
# kirajzolásig eltelt időt méri, majd 2 mp múlva (az előtöltés után) a memóriát, és kilép.
param(
    [Parameter(Mandatory = $true)][string]$Image,
    [int]$Runs = 5,
    [string]$Exe = "$PSScriptRoot\..\target\release\fastpic.exe"
)

$Exe = (Resolve-Path $Exe).Path
$Image = (Resolve-Path $Image).Path
$out = Join-Path $env:TEMP "fastpic-bench-$PID.txt"
$results = @()

# Első, "hideg" futás: a lemezgyorsítótár feltöltése, nem számít bele.
Start-Process -FilePath $Exe -ArgumentList "--bench `"$out`" `"$Image`"" -Wait
for ($i = 1; $i -le $Runs; $i++) {
    Remove-Item $out -ErrorAction SilentlyContinue
    Start-Process -FilePath $Exe -ArgumentList "--bench `"$out`" `"$Image`"" -Wait
    $kv = @{}
    Get-Content $out | ForEach-Object { $k, $v = $_ -split '=', 2; $kv[$k] = [double]$v }
    $results += [pscustomobject]$kv
    "{0}. futás: első kép {1,6:N1} ms   working set {2,6:N1} MB   csúcs {3,6:N1} MB   private {4,6:N1} MB" -f `
        $i, $kv.first_image_ms, $kv.working_set_mb, $kv.peak_working_set_mb, $kv.private_mb
}
Remove-Item $out -ErrorAction SilentlyContinue

$sorted = $results.first_image_ms | Sort-Object
$median = $sorted[[math]::Floor($sorted.Count / 2)]
""
"Medián első kép:  {0:N1} ms   (cél: < 100 ms)" -f $median
"Átlag working set: {0:N1} MB   (cél: < 30 MB)" -f ($results.working_set_mb | Measure-Object -Average).Average
"Exe méret:         {0:N2} MB   (cél: < 6 MB)" -f ((Get-Item $Exe).Length / 1MB)
