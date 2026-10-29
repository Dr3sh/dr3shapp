"""Minecraft + mod loader install and launch via minecraft-launcher-lib.

PROTOTYPE: offline/username mode only. Microsoft login is the next step.
Libraries/assets/Java runtime live in a shared folder; every server has its own game dir.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import uuid
from pathlib import Path
from typing import Callable

from .config import Paths, Settings

StatusCb = Callable[[str, float | None], None]   # text, fraction


class LaunchError(Exception):
    pass


# Default high-performance GC flags for fast mod loading (matches Prism Launcher & Aikar client profile)
DEFAULT_CLIENT_JVM_FLAGS = [
    "-XX:+UseG1GC",
    "-XX:+ParallelRefProcEnabled",
    "-XX:MaxGCPauseMillis=50",
    "-XX:+UnlockExperimentalVMOptions",
    "-XX:+DisableExplicitGC",
    "-XX:+AlwaysPreTouch",  # Prism key feature: pre-touches memory pages in parallel at boot (eliminates lag spikes while loading mods/textures)
    "-XX:G1NewSizePercent=28",
    "-XX:G1MaxNewSizePercent=40",
    "-XX:G1ReservePercent=15",
    "-XX:G1HeapWastePercent=5",
    "-XX:G1MixedGCCountTarget=4",
    "-XX:InitiatingHeapOccupancyPercent=20",
    "-XX:G1MixedGCLiveThresholdPercent=90",
    "-XX:G1RSetUpdatingPauseTimePercent=5",
    "-XX:SurvivorRatio=32",
    "-XX:+PerfDisableSharedMem",
    "-XX:MaxTenuringThreshold=1",
    "-Dsun.rmi.dgc.server.gcInterval=2147483646",
    "-Dfml.ignoreInvalidMinecraftCertificates=true",
    "-Dfml.ignorePatchDiscrepancies=true",
    "-Djava.net.preferIPv4Stack=true",
    "-Dminecraft.launcher.brand=Dr3shLauncher",
    "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump",
]


def offline_uuid(name: str) -> str:
    h = bytearray(hashlib.md5(("OfflinePlayer:" + name).encode()).digest())
    h[6] = (h[6] & 0x0F) | 0x30
    h[8] = (h[8] & 0x3F) | 0x80
    return str(uuid.UUID(bytes=bytes(h)))


def _import_mll():
    try:
        import minecraft_launcher_lib as mll
        return mll
    except ImportError as e:
        raise LaunchError("minecraft-launcher-lib is not installed (pip install minecraft-launcher-lib)") from e


def ensure_java_runtime(mll, shared: str, component: str = "java-runtime-gamma",
                        callback: dict | None = None, status: StatusCb | None = None) -> str:
    java_path = mll.runtime.get_executable_path(component, shared)
    if java_path and Path(java_path).is_file():
        return java_path
    if status:
        status("Descargando Java automáticamente", None)
    try:
        mll.runtime.install_jvm_runtime(component, shared, callback=callback)
        java_path = mll.runtime.get_executable_path(component, shared)
        if java_path and Path(java_path).is_file():
            return java_path
    except Exception as exc:
        system_java = shutil.which("javaw") or shutil.which("java")
        if system_java:
            return system_java
        raise LaunchError(
            "No se pudo descargar Java automáticamente. Comprueba tu conexión a Internet y vuelve a pulsar Jugar."
        ) from exc
    raise LaunchError(
        "La descarga de Java terminó, pero no se encontró su ejecutable. Vuelve a pulsar Jugar para intentarlo otra vez."
    )


def _java_component(shared: str, minecraft_version: str) -> str:
    version_json = Path(shared) / "versions" / minecraft_version / f"{minecraft_version}.json"
    try:
        with version_json.open("r", encoding="utf-8") as file:
            version_data = json.load(file)
        component = version_data.get("javaVersion", {}).get("component")
        return str(component) if component else "java-runtime-gamma"
    except (OSError, ValueError, AttributeError):
        return "java-runtime-gamma"


_VERSION_ID_CACHE: dict[str, str] = {}


def _find_version_id(mll, shared: str, loader: str, mc: str, loader_version: str | None = None) -> str:
    cache_key = f"{loader}:{mc}:{loader_version or ''}"
    if cache_key in _VERSION_ID_CACHE:
        return _VERSION_ID_CACHE[cache_key]

    ids = [v["id"] for v in mll.utils.get_installed_versions(shared)]
    if loader in ("", "vanilla"):
        pick = [i for i in ids if i == mc]
    else:
        pick = []
        if loader_version:
            try:
                profile_id = mll.mod_loader.get_mod_loader(loader).get_installed_version(mc, loader_version)
                if profile_id in ids:
                    pick = [profile_id]
            except Exception:
                pass
        if not pick:
            # Fallback to any installed profile matching loader and minecraft version
            pick = [i for i in ids if loader in i.lower() and mc in i]
    if not pick:
        raise LaunchError(f"Installed version for {loader or 'vanilla'} {mc} not found")
    _VERSION_ID_CACHE[cache_key] = pick[-1]
    return pick[-1]


def ensure_persistent_natives(mll, version_id: str, shared: str) -> str:
    """Ensure natives are extracted to a persistent folder once (Prism Launcher architecture).
    Avoids Windows Defender re-scanning newly created temp DLLs on each launch."""
    natives_dir = Path(shared) / "versions" / version_id / "natives"
    natives_dir.mkdir(parents=True, exist_ok=True)
    dlls = list(natives_dir.glob("*.dll"))
    if len(dlls) >= 4:
        return str(natives_dir.resolve())
    try:
        mll.natives.extract_natives(version_id, shared, str(natives_dir))
    except Exception:
        pass
    return str(natives_dir.resolve())


def resolve_loader_version(mll, loader: str, mc_version: str, preferred_version: str | None = None) -> str | None:
    loader = (loader or "").strip().lower()
    if not loader or loader in ("vanilla", "none"):
        return None
    if preferred_version and loader != "fabric":
        return preferred_version

    try:
        versions = mll.mod_loader.get_mod_loader(loader).get_loader_versions(mc_version, stable_only=True)
        return str(versions[0]) if versions else None
    except Exception:
        if loader == "fabric":
            try:
                return str(mll.fabric.get_latest_loader_version())
            except Exception:
                pass
        return None


def _is_loader_installed(mll, shared: str, loader: str, mc: str, loader_version: str | None) -> bool:
    ids = [str(v["id"]).lower() for v in mll.utils.get_installed_versions(shared)]
    if loader in ("", "vanilla"):
        return any(i == mc.lower() for i in ids)
    if not loader_version:
        return any(loader.lower() in i and mc.lower() in i for i in ids)
    try:
        profile_id = mll.mod_loader.get_mod_loader(loader).get_installed_version(mc, loader_version)
        if profile_id.lower() in ids:
            return True
    except Exception:
        pass
    return any(loader.lower() in i and mc.lower() in i for i in ids)


def _clean_download_message(raw_text: str, default_label: str) -> str:
    text = str(raw_text or "").strip()
    if not text:
        return default_label

    if text.lower().startswith("download "):
        rest = text[9:].strip()
        if re.fullmatch(r"[0-9a-fA-F]{6,64}", rest) or (rest.isdigit() and len(rest) >= 6):
            return default_label

    text = re.sub(r"\b[0-9a-fA-F]{8,64}\b", "", text).strip()
    text = re.sub(r"\s+", " ", text).strip(" -:")
    if not text:
        return default_label

    lower = text.lower()
    if "downloading assets" in lower or "download assets" in lower or "assets" in lower:
        return f"{default_label} (recursos)"
    if "libraries" in lower or "library" in lower:
        return f"{default_label} (librerías)"
    if "client" in lower:
        return f"{default_label} (cliente)"
    if "extract" in lower:
        return f"{default_label} (extrayendo)"

    return text


def is_minecraft_complete(shared: str, mc: str) -> bool:
    shared_path = Path(shared)
    version_dir = shared_path / "versions" / mc
    version_json = version_dir / f"{mc}.json"
    client_jar = version_dir / f"{mc}.jar"
    verified_marker = version_dir / ".verified"

    if verified_marker.is_file() and version_json.is_file() and client_jar.is_file():
        return True

    if not version_json.is_file() or not client_jar.is_file():
        return False
    try:
        with version_json.open("r", encoding="utf-8") as f:
            vdata = json.load(f)
        asset_id = str(vdata.get("assetIndex", {}).get("id") or vdata.get("assets") or "").strip()
        if asset_id:
            index_file = shared_path / "assets" / "indexes" / f"{asset_id}.json"
            if not index_file.is_file():
                return False
        objects_dir = shared_path / "assets" / "objects"
        if not objects_dir.is_dir():
            return False
        subdirs = [p for p in objects_dir.iterdir() if p.is_dir()]
        if len(subdirs) < 50:
            return False
        try:
            verified_marker.write_text("ok", encoding="utf-8")
        except OSError:
            pass
    except Exception:
        return False
    return True


def ensure_minecraft_installed(mll, shared: str, mc: str, callback: dict | None = None, status: StatusCb | None = None) -> None:
    if is_minecraft_complete(shared, mc):
        return
    if status:
        status(f"Descargando Minecraft {mc} y recursos", None)
    installer = getattr(mll, "install", mll)
    install_fn = getattr(installer, "install_minecraft_version", None)
    if install_fn:
        install_fn(mc, shared, callback=callback)


def _create_mll_callback(label: str, status: Callable[[str, float | None], None]) -> dict:
    state = {"max": 1, "last_msg": label}

    def on_set_status(t: str):
        clean = _clean_download_message(t, label)
        state["last_msg"] = clean
        status(clean, None)

    def on_set_progress(p: int):
        status(state["last_msg"], min(max(float(p) / float(state["max"]), 0.0), 1.0))

    def on_set_max(m: int):
        state["max"] = max(int(m or 1), 1)

    return {
        "setStatus": on_set_status,
        "setMax": on_set_max,
        "setProgress": on_set_progress,
    }


def prepare_and_launch(
    paths: Paths,
    settings: Settings,
    version_meta: dict,
    game_dir: Path,
    status: Callable[[str, float | None], None],
    *,
    min_ram_mb: int | None = None,
    account: dict | None = None,
    ram_mb: int | None = None,
    microsoft_client_id: str = "",
) -> subprocess.Popen:
    mll = _import_mll()
    mc = (version_meta.get("minecraft_version") or "").strip()
    loader = (version_meta.get("mod_loader") or "").strip().lower()
    loader_version = (version_meta.get("mod_loader_version") or "").strip()
    if not mc:
        raise LaunchError("This version has no 'minecraft_version' in versions.json "
                          "(add it, see the project document section 8.2)")

    account = account or settings.current_account()
    account_mode = str(account.get("mode") or settings["account_mode"] or "offline").lower()
    name = (account.get("username") or settings["account_username"] or settings["username"] or "Player").strip() or "Player"

    shared = str(paths.shared)
    mc_cb = _create_mll_callback(f"Descargando Minecraft {mc}", status)
    ensure_minecraft_installed(mll, shared, mc, callback=mc_cb, status=status)

    status("Comprobando runtime de Java", None)
    custom_java = str(settings["java_path"] or "").strip()
    java_path = ""
    if custom_java and Path(custom_java).is_file():
        java_path = str(Path(custom_java).resolve())
    else:
        # 1. Check local shared runtime
        comp = _java_component(shared, mc)
        local_java = mll.runtime.get_executable_path(comp, shared)
        if local_java and Path(local_java).is_file():
            java_path = local_java
        else:
            java_cb = _create_mll_callback("Descargando runtime de Java", status)
            java_path = ensure_java_runtime(mll, shared, comp, callback=java_cb, status=status)

    # Prefer javaw.exe on Windows for native GUI launch without console overhead (just like Prism)
    if java_path and sys.platform.startswith("win"):
        p_java = Path(java_path)
        if p_java.name.lower() == "java.exe":
            javaw = p_java.with_name("javaw.exe")
            if javaw.is_file():
                java_path = str(javaw.resolve())

    # Fast offline loader check: if installed locally, avoid synchronous API calls to meta.fabricmc.net
    version_id = None
    if loader in ("", "vanilla"):
        version_id = mc
    else:
        try:
            version_id = _find_version_id(mll, shared, loader, mc, loader_version or None)
        except Exception:
            version_id = None

    if loader not in ("", "vanilla") and not version_id:
        resolved_loader_version = loader_version or resolve_loader_version(mll, loader, mc, loader_version)
        if not resolved_loader_version:
            raise LaunchError(f"No compatible {loader.upper()} version found for Minecraft {mc}")
        if loader in {"fabric", "forge", "neoforge", "quilt"}:
            loader_cb = _create_mll_callback(f"Descargando {loader.upper()} {resolved_loader_version}", status)
            status(f"Descargando {loader.upper()} {resolved_loader_version}", None)
            mll.mod_loader.get_mod_loader(loader).install(
                mc, shared, loader_version=resolved_loader_version, callback=loader_cb, java=java_path,
            )
            version_id = _find_version_id(mll, shared, loader, mc, resolved_loader_version)
        else:
            raise LaunchError(f"Mod loader '{loader}' is not supported in this prototype yet")

    ram = max(int(ram_mb or settings["ram_mb"]), int(min_ram_mb or 0))
    configured_min_ram = int(settings["min_ram_mb"] or 1024)
    # Prism architecture: for heaps >= 4GB with AlwaysPreTouch, allocate initial heap to 3/4 or full RAM
    # to eliminate runtime heap expansion pauses while loading textures and mod assets.
    if ram >= 4096 and configured_min_ram <= 1024:
        initial_ram = max(2048, (ram * 3) // 4)
    else:
        initial_ram = max(512, configured_min_ram)
    initial_ram = min(ram, initial_ram)

    jvm_arguments = [f"-Xmx{ram}M", f"-Xms{initial_ram}M"]

    # G1 Heap Region Size optimization:
    # Large modpacks (Cobblemon) allocate large byte arrays for textures/models.
    # Default 1MB regions lead to frequent humongous allocations and GC freezes.
    if ram >= 8192:
        jvm_arguments.append("-XX:G1HeapRegionSize=16M")
    elif ram >= 4096:
        jvm_arguments.append("-XX:G1HeapRegionSize=8M")

    extra_jvm = str(settings["jvm_args"] or "").strip()
    if extra_jvm:
        for arg in extra_jvm.split():
            if not arg.startswith(("-Xmx", "-Xms")):
                jvm_arguments.append(arg)
    else:
        jvm_arguments.extend(DEFAULT_CLIENT_JVM_FLAGS)

    extra_options: dict = {}
    # Use persistent natives directory to avoid re-extracting and re-scanning DLLs on every launch
    natives_dir = ensure_persistent_natives(mll, version_id, shared)
    extra_options["nativesDirectory"] = natives_dir

    win_w = int(settings["window_width"] or 854)
    win_h = int(settings["window_height"] or 480)
    if (win_w, win_h) != (854, 480):
        extra_options["customResolution"] = True
        extra_options["resolutionWidth"] = str(win_w)
        extra_options["resolutionHeight"] = str(win_h)

    if account_mode == "microsoft":
        token = str(account.get("access_token") or account.get("token") or "").strip()
        user_uuid = str(account.get("uuid") or "").strip()
        user_name = str(account.get("username") or name).strip()
        refresh_token = str(account.get("refresh_token") or "").strip()
        client_id = str(microsoft_client_id or account.get("client_id") or "00000000402b5328").strip() or "00000000402b5328"

        from .microsoft import is_minecraft_token_valid
        valid = bool(token and user_uuid and user_name and is_minecraft_token_valid(token))

        if not valid:
            if not refresh_token:
                raise LaunchError("Selecciona una cuenta Microsoft válida y vuelve a iniciar sesión.")
            from .microsoft import refresh_microsoft_session
            profile = None
            last_exc = None
            for candidate_client_id in ([client_id] if client_id != "00000000402b5328" else ["00000000402b5328", "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb"]):
                try:
                    profile = refresh_microsoft_session(candidate_client_id, refresh_token, mll=mll)
                    break
                except Exception as exc:
                    last_exc = exc
            if profile is None:
                raise LaunchError("No se pudo renovar la cuenta Microsoft. Vuelve a iniciar sesión desde Ajustes.") from last_exc
            token = str(profile.get("access_token") or "").strip()
            if not token or not profile.get("id") or not profile.get("name"):
                raise LaunchError("Microsoft no devolvió un perfil de Minecraft válido. Vuelve a iniciar sesión.")
            user_name = profile["name"]
            user_uuid = profile["id"]
            refresh_token = profile.get("refresh_token") or refresh_token
            account.update({
                "username": user_name,
                "uuid": user_uuid,
                "refresh_token": refresh_token,
                "access_token": token,
                "token": token,
            })

        name = user_name
        options = {
            "username": name,
            "uuid": user_uuid,
            "token": token,
            "executablePath": java_path,
            "gameDirectory": str(game_dir),
            "jvmArguments": jvm_arguments,
            **extra_options,
        }
        status("Iniciando Minecraft con cuenta Microsoft…", None)
    else:
        options = {
            "username": name,
            "uuid": offline_uuid(name),
            "token": "",
            "executablePath": java_path,
            "gameDirectory": str(game_dir),
            "jvmArguments": jvm_arguments,
            **extra_options,
        }
        status("Iniciando Minecraft en modo sin conexión…", None)

    command = list(mll.command.get_minecraft_command(version_id, shared, options))
    if bool(settings["fullscreen"]) and "--fullscreen" not in command:
        command.append("--fullscreen")
    game_dir.mkdir(parents=True, exist_ok=True)
    try:
        from .skins import ensure_custom_skin_loader
        ensure_custom_skin_loader(paths, settings, game_dir, mc, loader, name, status=status)
    except Exception:
        pass
    creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    if sys.platform.startswith("win"):
        # 0x00008000 = ABOVE_NORMAL_PRIORITY_CLASS (Prism Launcher process priority for maximum FPS & responsiveness)
        creationflags |= 0x00008000

    clean_env = os.environ.copy()
    for leak_var in ("PYTHONPATH", "PYTHONHOME", "ELECTRON_RUN_AS_NODE", "NODE_OPTIONS"):
        clean_env.pop(leak_var, None)

    return subprocess.Popen(command, cwd=str(game_dir), creationflags=creationflags, env=clean_env)
