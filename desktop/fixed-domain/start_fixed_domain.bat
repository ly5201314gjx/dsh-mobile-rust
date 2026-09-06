@echo off
chcp 65001 >nul
cd /d "%~dp0"
echo ============================================
echo   DSH Desktop - fixed domain (own domain) launcher
echo ============================================
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start_fixed_domain.ps1" -TunnelName dsh-lg
echo.
echo --------------------------------------------------
echo  Keep this window OPEN to keep the tunnels running.
echo --------------------------------------------------
pause