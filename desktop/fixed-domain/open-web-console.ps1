$ErrorActionPreference='Continue'
param([string]$Bundle = $PSScriptRoot,[string]$Config = (Join-Path $PSScriptRoot 'config.yml'))
$running = ((Get-Process dsh-desktop,cloudflared -ErrorAction SilentlyContinue).Count -gt 0)
if(-not $running){
  Write-Host 'DSH remote is not running - starting the whole stack now...'
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $Bundle 'start_fixed_domain.ps1')
}
$web = (Select-String -Path $Config -Pattern '^\s*-\s*hostname:\s*([^\s]+)' -ErrorAction SilentlyContinue |
  ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() })
if($web.Count -ge 2){ $hostB=$web[1] } else { Write-Host 'web hostname not found'; pause; exit 1 }
$url = "https://$hostB/"
Write-Host ''
Write-Host "Opening DSH Web Console:  $url"
Start-Process $url