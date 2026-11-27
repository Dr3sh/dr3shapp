@echo off
title Dr3sh App - Modo Desarrollo
cd /d "E:\launcehr\Dr3sh app"

set "GRADLE_USER_HOME=E:\.gradle"
set "JAVA_HOME=C:\Program Files\Eclipse Adoptium\jdk-21.0.12.101-hotspot"
set "CARGO_HOME=E:\.cargo"
set "CARGO_TARGET_DIR=E:\cargo-target-dr3shapp"
set "CC=E:\LLVM\bin\clang-cl.exe"
set "CXX=E:\LLVM\bin\clang-cl.exe"
set "INCLUDE=E:\xwin-sdk\crt\include;E:\xwin-sdk\sdk\include\ucrt;E:\xwin-sdk\sdk\include\um;E:\xwin-sdk\sdk\include\shared"
set "LIB=E:\xwin-sdk\crt\lib\x64;E:\xwin-sdk\sdk\lib\um\x64;E:\xwin-sdk\sdk\lib\ucrt\x64"
set "PATH=C:\Program Files\Eclipse Adoptium\jdk-21.0.12.101-hotspot\bin;E:\LLVM\bin;E:\Git\cmd;%PATH%"

set "LOG_FILE=E:\launcehr\Dr3sh app\dr3sh-dev-last.log"

echo ========================================================
echo   Iniciando Dr3sh App (Tauri v2 + Vue 3)
echo   Log en vivo: %LOG_FILE%
echo ========================================================
> "%LOG_FILE%" echo === Dr3sh App Dev Log - Inicio: %date% %time% ===
(pnpm --config.pm-on-fail=ignore --filter @modrinth/app dev 2>&1) | powershell -NoProfile -Command "$sw = [System.IO.StreamWriter]::new($env:LOG_FILE, $true, [System.Text.UTF8Encoding]::new($false)); $sw.AutoFlush = $true; try { while ($null -ne ($line = [Console]::In.ReadLine())) { [Console]::WriteLine($line); $sw.WriteLine($line) } } finally { $sw.Dispose() }"
pause
