# 固定域名（自有域名）一键启动：命名隧道 + dsh-desktop + token 代理 + 打印配对链接
# 前提：当前脚本所在目录放有 dsh-desktop.exe / node.exe / cloudflared.exe /
#       dsh-token-proxy.js / payload\dsh-app / dsh-home ，以及按所需定制的 config.yml
#       （见 SETUP_OWN_DOMAIN.md；隧道名默认 dsh-lg，可用 -TunnelName 覆盖）
param(
  [string]$Bundle = $PSScriptRoot,
  [string]$Config = (Join-Path $PSScriptRoot 'config.yml'),
  [string]$TunnelName = 'dsh-lg'
)
$ErrorActionPreference = 'Continue'
$node   = Join-Path $Bundle 'node.exe'
$cf     = Join-Path $Bundle 'cloudflared.exe'
$desktop= Join-Path $Bundle 'dsh-desktop.exe'
$proxy  = Join-Path $Bundle 'dsh-token-proxy.js'
$home   = Join-Path $Bundle 'dsh-home'
$dlog   = Join-Path $Bundle 'desktop-run.log'

if(-not (Test-Path $Config)){ Write-Host "ERROR: config.yml not found: $Config"; exit 1 }
$hosts = @(Select-String -Path $Config -Pattern '^\s*-\s*hostname:\s*([^\s]+)' -ErrorAction SilentlyContinue |
  ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() })
if($hosts.Count -lt 2){ Write-Host "ERROR: config.yml 至少需要两个 hostname（pair / web）"; exit 1 }
$PAIR_HOST = $hosts[0]; $WEB_HOST = $hosts[1]
Write-Host "Fixed-domain launcher -> pair=$PAIR_HOST web=$WEB_HOST tunnel=$TunnelName"

# --- 清理残留 ---
Get-Process dsh-desktop,cloudflared -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800
foreach($port in 3090,3080,5780){
  Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $_.LocalPort -eq $port } | ForEach-Object {
    Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue
  }
}
Start-Sleep -Milliseconds 600
Remove-Item (Join-Path $Bundle 'desktop-run.log') -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $home 'dsh-desktop-node.log') -Force -ErrorAction SilentlyContinue

# --- 启动命名隧道（自有域名，config 里已写 ingress）---
Start-Process $cf -ArgumentList 'tunnel','--config',$Config,'--no-autoupdate','run',$TunnelName -RedirectStandardOutput (Join-Path $Bundle 'cf.log') -RedirectStandardError (Join-Path $Bundle 'cf.err') -WindowStyle Hidden
Start-Sleep -Seconds 6

# --- 启动 dsh-desktop（隧道模式，广播固定域名）---
Start-Process $desktop -ArgumentList '--node',$node,'--app-dir',(Join-Path $Bundle 'payload\dsh-app'),'--home',$home,'--advertise',("wss://"+$PAIR_HOST),'--advertise-web',$WEB_HOST -RedirectStandardOutput $dlog -WindowStyle Hidden

# --- 等待 web token + 配对 key ---
$token=$null; $key=$null
for($i=0;$i -lt 40;$i++){
  Start-Sleep -Milliseconds 500
  if(-not $token){
    $m = Select-String -Path (Join-Path $home 'dsh-desktop-node.log') -Pattern '\?token=([^\s]+)' -ErrorAction SilentlyContinue | Select-Object -Last 1
    if($m){ $token = $m.Matches[0].Groups[1].Value }
  }
  if(-not $key){
    $m = Select-String -Path $dlog -Pattern 'key=([0-9a-f]{40})' -ErrorAction SilentlyContinue | Select-Object -Last 1
    if($m){ $key = $m.Matches[0].Groups[1].Value }
  }
  if($token -and $key){ break }
}
if(-not $token -or -not $key){ Write-Host 'ERROR: failed to obtain web token / pairing key.'; exit 1 }

# --- 启动 token 注入代理 ---
$env:PROXY_TOKEN = $token; $env:UP_HOST='127.0.0.1'; $env:UP_PORT='3080'; $env:PORT='3090'
Start-Process $node -ArgumentList $proxy -WindowStyle Hidden
Start-Sleep -Seconds 2

Write-Host ''
Write-Host '=================================================='
Write-Host ' DSH Desktop remote READY (fixed domain).'
Write-Host ' Pairing link (scan / paste in app):'
Write-Host "   dsh-link-wss://$PAIR_HOST/#key=$key&web=$WEB_HOST"
Write-Host ' Pairing page (open on phone):'
Write-Host "   https://$PAIR_HOST/pair"
Write-Host ' Computer web console (token auto-injected):'
Write-Host "   https://$WEB_HOST/"
Write-Host '=================================================='
Write-Host ''