@echo off
title Dr3sh App - Empaquetando Launcher
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
set "TAURI_SIGNING_PRIVATE_KEY=E:\launcehr\Dr3sh app\apps\app\dr3sh_updater.key"
set "TAURI_SIGNING_PRIVATE_KEY_PASSWORD="

pnpm --config.pm-on-fail=ignore --filter @modrinth/app build
