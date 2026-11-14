# Desenvolvimento: compila e instala a versão local.
# O op2.exe se instala sozinho em %LOCALAPPDATA%\Programs\OP2 e cria o atalho.
# Uso:  powershell -ExecutionPolicy Bypass -File install.ps1
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# fecha a versão aberta (o cargo não consegue sobrescrever um .exe em uso)
Get-Process op2 -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'Falha ao compilar' }

Start-Process 'target\release\op2.exe'
