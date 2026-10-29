"""Custom skin management for offline accounts and CustomSkinLoader multiplayer sync."""
from __future__ import annotations

import base64
import json
import re
import shutil
import struct
from pathlib import Path
from typing import Callable

import requests

from .config import Paths, Settings, write_json

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
MODRINTH_CSL_VERSIONS_URL = "https://api.modrinth.com/v2/project/customskinloader/version"
DR3SH_SKINS_BASE_URL = "https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/refs/heads/main/skins/"


def validate_skin_png(data: bytes) -> tuple[int, int]:
    """Verify data is a valid PNG skin image and return (width, height)."""
    if not data or len(data) < 33 or not data.startswith(PNG_SIGNATURE):
        raise ValueError("El archivo seleccionado no es una imagen PNG válida.")
    if data[12:16] != b"IHDR":
        raise ValueError("El archivo PNG no tiene una cabecera válida.")
    width, height = struct.unpack(">II", data[16:24])
    if width < 64 or height < 32 or width > 1024 or height > 1024 or width % 64 != 0 or (height != width and height * 2 != width):
        raise ValueError("La skin debe ser un PNG de 64x64 o 64x32 píxeles (o proporción equivalente HD).")
    return width, height


def normalize_skin_model(model: str | None) -> str:
    val = str(model or "").strip().lower()
    return "slim" if val in {"slim", "alex", "3px"} else "classic"


def active_skin_file(paths: Paths, settings: Settings, username: str = "") -> Path | None:
    configured = str(settings["offline_skin_path"] or "").strip()
    if configured:
        candidate = Path(configured)
        if candidate.is_file():
            return candidate
    clean_name = re.sub(r"[^A-Za-z0-9_]", "", str(username or settings["username"] or "").strip())
    if clean_name:
        by_name = paths.skins / f"{clean_name}.png"
        if by_name.is_file():
            return by_name
    default_file = paths.skins / "offline_skin.png"
    if default_file.is_file():
        return default_file
    return None


def get_offline_skin_info(paths: Paths, settings: Settings, username: str = "") -> dict:
    skin_file = active_skin_file(paths, settings, username=username)
    model = normalize_skin_model(settings["offline_skin_model"])
    custom_mod = bool(settings["custom_skin_mod"])
    if skin_file is None:
        return {
            "has_skin": False,
            "skin_path": "",
            "skin_data_url": "",
            "data_url": "",
            "skin_model": model,
            "model": model,
            "custom_skin_mod": custom_mod,
        }
    try:
        raw = skin_file.read_bytes()
        validate_skin_png(raw)
        encoded = base64.b64encode(raw).decode("ascii")
        data_url = f"data:image/png;base64,{encoded}"
        return {
            "has_skin": True,
            "skin_path": str(skin_file.resolve()),
            "skin_data_url": data_url,
            "data_url": data_url,
            "skin_model": model,
            "model": model,
            "custom_skin_mod": custom_mod,
        }
    except Exception:
        return {
            "has_skin": False,
            "skin_path": "",
            "skin_data_url": "",
            "data_url": "",
            "skin_model": model,
            "model": model,
            "custom_skin_mod": custom_mod,
        }


def build_csl_config(model: str = "classic") -> dict:
    csl_model = "slim" if normalize_skin_model(model) == "slim" else "default"
    return {
        "enable": True,
        "enableDynamicSkull": True,
        "enableTransparentSkin": True,
        "forceLoadAllTextures": True,
        "enableCape": True,
        "cacheExpiry": 10,
        "loadlist": [
            {
                "name": "LocalSkin",
                "type": "Legacy",
                "checkPNG": False,
                "skin": "LocalSkin/skins/{USERNAME}.png",
                "model": csl_model,
                "cape": "LocalSkin/capes/{USERNAME}.png",
                "elytra": "LocalSkin/elytras/{USERNAME}.png",
            },
            {
                "name": "Dr3shSkins",
                "type": "Legacy",
                "checkPNG": False,
                "skin": f"{DR3SH_SKINS_BASE_URL}{{USERNAME}}.png",
                "model": "auto",
                "cape": f"{DR3SH_SKINS_BASE_URL}capes/{{USERNAME}}.png",
            },
            {
                "name": "Mojang",
                "type": "MojangAPI",
                "apiRoot": "https://api.mojang.com/",
                "sessionRoot": "https://sessionserver.mojang.com/",
            },
            {
                "name": "ElyBy",
                "type": "ElyByAPI",
                "root": "http://skinsystem.ely.by/textures/",
            },
            {
                "name": "LittleSkin",
                "type": "CustomSkinAPI",
                "root": "https://littleskin.cn/csl/",
            },
            {
                "name": "Glitchless",
                "type": "GlitchlessAPI",
                "root": "https://games.glitchless.ru/api/minecraft/users/profiles/textures/?nickname=",
            },
        ],
    }


def apply_skin_to_instance(paths: Paths, settings: Settings, game_dir: Path, username: str = "") -> None:
    """Write CustomSkinLoader config and copy the active offline skin into an instance directory."""
    if not game_dir:
        return
    clean_name = re.sub(r"[^A-Za-z0-9_]", "", str(username or settings["username"] or "Player").strip()) or "Player"
    model = normalize_skin_model(settings["offline_skin_model"])

    csl_dir = game_dir / "CustomSkinLoader"
    local_skins_dir = csl_dir / "LocalSkin" / "skins"
    local_skins_dir.mkdir(parents=True, exist_ok=True)
    write_json(csl_dir / "CustomSkinLoader.json", build_csl_config(model))

    skin_file = active_skin_file(paths, settings, username=clean_name)
    if skin_file and skin_file.is_file():
        for target_name in {f"{clean_name}.png", f"{clean_name.lower()}.png"}:
            dest = local_skins_dir / target_name
            try:
                if dest.resolve() != skin_file.resolve():
                    shutil.copyfile(skin_file, dest)
            except OSError:
                pass
        offline_skins_dir = game_dir / "Offlineskins" / "skins"
        try:
            offline_skins_dir.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(skin_file, offline_skins_dir / f"{clean_name}.png")
        except OSError:
            pass

        dr3sh_skins_dir = game_dir / "dr3sh_skins"
        try:
            dr3sh_skins_dir.mkdir(parents=True, exist_ok=True)
            for target_name in {f"{clean_name}.png", f"{clean_name.lower()}.png"}:
                shutil.copyfile(skin_file, dr3sh_skins_dir / target_name)
        except OSError:
            pass
    else:
        for target_name in {f"{clean_name}.png", f"{clean_name.lower()}.png"}:
            (local_skins_dir / target_name).unlink(missing_ok=True)
            (game_dir / "dr3sh_skins" / target_name).unlink(missing_ok=True)


def sync_skin_to_all_instances(paths: Paths, settings: Settings, username: str = "") -> None:
    instances_base = settings.instances_base()
    if not instances_base.is_dir():
        return
    for child in instances_base.iterdir():
        if child.is_dir():
            try:
                apply_skin_to_instance(paths, settings, child, username=username)
            except Exception:
                continue


def save_offline_skin_bytes(paths: Paths, settings: Settings, png_bytes: bytes, username: str = "", model: str = "") -> dict:
    validate_skin_png(png_bytes)
    paths.skins.mkdir(parents=True, exist_ok=True)
    clean_name = re.sub(r"[^A-Za-z0-9_]", "", str(username or settings["username"] or "Player").strip()) or "Player"
    normalized_model = normalize_skin_model(model or settings["offline_skin_model"])

    default_target = paths.skins / "offline_skin.png"
    default_target.write_bytes(png_bytes)
    named_target = paths.skins / f"{clean_name}.png"
    named_target.write_bytes(png_bytes)

    settings["offline_skin_path"] = str(named_target.resolve())
    settings["offline_skin_model"] = normalized_model
    settings.save()
    sync_skin_to_all_instances(paths, settings, username=clean_name)
    return get_offline_skin_info(paths, settings, username=clean_name)


def _fetch_mojang_skin(username: str) -> tuple[bytes | None, str | None]:
    try:
        prof_res = requests.get(
            f"https://api.mojang.com/users/profiles/minecraft/{username}",
            headers={"User-Agent": "Dr3shLauncher"},
            timeout=10,
        )
        if not prof_res.ok or not prof_res.text:
            return None, None
        uuid = prof_res.json().get("id")
        if not uuid:
            return None, None
        sess_res = requests.get(
            f"https://sessionserver.mojang.com/session/minecraft/profile/{uuid}",
            headers={"User-Agent": "Dr3shLauncher"},
            timeout=10,
        )
        if not sess_res.ok or not sess_res.text:
            return None, None
        props = sess_res.json().get("properties") or []
        for prop in props:
            if prop.get("name") == "textures" and prop.get("value"):
                decoded = json.loads(base64.b64decode(prop["value"]).decode("utf-8"))
                skin_obj = (decoded.get("textures") or {}).get("SKIN") or {}
                skin_url = skin_obj.get("url")
                if not skin_url:
                    continue
                skin_url = str(skin_url).replace("http://textures.minecraft.net", "https://textures.minecraft.net")
                meta_model = ((skin_obj.get("metadata") or {}).get("model") or "classic").lower()
                img_res = requests.get(skin_url, headers={"User-Agent": "Dr3shLauncher"}, timeout=12)
                if img_res.ok and img_res.content:
                    return img_res.content, ("slim" if meta_model == "slim" else "classic")
    except Exception:
        pass
    return None, None


def import_offline_skin(paths: Paths, settings: Settings, source: str, username: str = "", model: str = "") -> dict:
    raw_source = str(source or "").strip()
    if not raw_source:
        raise ValueError("Indica un archivo PNG, nombre de jugador o URL de skin.")

    detected_model = model
    png_bytes: bytes | None = None

    local_path = Path(raw_source)
    if local_path.is_file():
        png_bytes = local_path.read_bytes()
    elif re.match(r"^https?://", raw_source, flags=re.IGNORECASE):
        r = requests.get(raw_source, headers={"User-Agent": "Dr3shLauncher"}, timeout=15)
        if not r.ok or not r.content:
            raise ValueError("No se pudo descargar la skin desde esa URL.")
        png_bytes = r.content
    else:
        if not re.fullmatch(r"[A-Za-z0-9_]{1,16}", raw_source):
            raise ValueError("Ingresa un nombre de jugador de Minecraft válido (1-16 caracteres) o una URL.")
        png_bytes, mojang_model = _fetch_mojang_skin(raw_source)
        if mojang_model and not detected_model:
            detected_model = mojang_model
        if not png_bytes:
            for fallback_url in (
                f"https://mc-heads.net/skin/{raw_source}",
                f"https://minotar.net/skin/{raw_source}",
            ):
                try:
                    r = requests.get(fallback_url, headers={"User-Agent": "Dr3shLauncher"}, timeout=12)
                    if r.ok and r.content and r.content.startswith(PNG_SIGNATURE):
                        png_bytes = r.content
                        break
                except Exception:
                    continue
        if not png_bytes:
            raise ValueError(f"No se encontró ninguna skin pública para el jugador '{raw_source}'.")

    return save_offline_skin_bytes(
        paths,
        settings,
        png_bytes,
        username=username or settings["username"],
        model=detected_model or settings["offline_skin_model"],
    )


def clear_offline_skin(paths: Paths, settings: Settings, username: str = "") -> dict:
    clean_name = re.sub(r"[^A-Za-z0-9_]", "", str(username or settings["username"] or "Player").strip()) or "Player"
    settings["offline_skin_path"] = ""
    settings.save()
    for target in (paths.skins / "offline_skin.png", paths.skins / f"{clean_name}.png"):
        target.unlink(missing_ok=True)
    sync_skin_to_all_instances(paths, settings, username=clean_name)
    return get_offline_skin_info(paths, settings, username=clean_name)


def upload_skin_for_server(paths: Paths, settings: Settings, username: str = "") -> dict:
    """Upload the active custom skin PNG so the player gets a ready-to-use /skin url command for SkinsRestorer servers."""
    skin_file = active_skin_file(paths, settings, username=username)
    if skin_file is None or not skin_file.is_file():
        raise ValueError("Primero selecciona o importa una skin PNG en Ajustes.")
    raw = skin_file.read_bytes()
    validate_skin_png(raw)
    model = normalize_skin_model(settings["offline_skin_model"])

    # Try MineSkin / public direct PNG hosts that return a direct .png URL usable with SkinsRestorer
    uploaders = (
        ("https://litterbox.catbox.moe/resources/internals/api.php", {"reqtype": "fileupload", "time": "72h"}, "fileToUpload"),
        ("https://catbox.moe/user/api.php", {"reqtype": "fileupload"}, "fileToUpload"),
        ("https://0x0.st", {}, "file"),
    )
    uploaded_url = ""
    for endpoint, data, field_name in uploaders:
        try:
            r = requests.post(
                endpoint,
                data=data,
                files={field_name: ("skin.png", raw, "image/png")},
                headers={"User-Agent": "Dr3shLauncher/1.0"},
                timeout=20,
            )
            text = (r.text or "").strip()
            if r.ok and re.match(r"^https?://\S+", text):
                uploaded_url = text
                break
        except Exception:
            continue

    if not uploaded_url:
        raise ValueError("No se pudo generar el enlace público de la skin en este momento.")

    sr_variant = "SLIM" if model == "slim" else "CLASSIC"
    command = f'/skin url "{uploaded_url}" {sr_variant}'
    return {
        "ok": True,
        "url": uploaded_url,
        "model": model,
        "command": command,
    }


def _select_csl_mod_asset(versions_payload: list, minecraft_version: str, mod_loader: str) -> tuple[str, str] | None:
    loader = str(mod_loader or "").strip().lower()
    mc = str(minecraft_version or "").strip()
    if not isinstance(versions_payload, list):
        return None

    # 1. Exact match on both loader and Minecraft version
    for entry in versions_payload:
        if not isinstance(entry, dict):
            continue
        loaders = [str(x).lower() for x in (entry.get("loaders") or [])]
        mc_versions = [str(x) for x in (entry.get("game_versions") or [])]
        if loader in loaders and (not mc or mc in mc_versions):
            for f in entry.get("files") or []:
                if isinstance(f, dict) and str(f.get("filename") or "").endswith(".jar") and f.get("url"):
                    return str(f["filename"]), str(f["url"])

    # 2. Fallback to latest Universal release supporting this loader
    for entry in versions_payload:
        if not isinstance(entry, dict):
            continue
        loaders = [str(x).lower() for x in (entry.get("loaders") or [])]
        if loader in loaders:
            for f in entry.get("files") or []:
                if isinstance(f, dict) and str(f.get("filename") or "").endswith(".jar") and f.get("url"):
                    return str(f["filename"]), str(f["url"])
    return None


def ensure_custom_skin_loader(
    paths: Paths,
    settings: Settings,
    game_dir: Path,
    minecraft_version: str,
    mod_loader: str,
    username: str,
    status: Callable[[str, float | None], None] | None = None,
) -> Path | None:
    """Ensure CustomSkinLoader configuration and mod jar exist in the instance so offline & premium players see all skins."""
    apply_skin_to_instance(paths, settings, game_dir, username=username)

    if not bool(settings["custom_skin_mod"]):
        return None

    loader = str(mod_loader or "").strip().lower()
    if loader not in {"fabric", "forge", "neoforge", "quilt"}:
        return None

    mods_dir = game_dir / "mods"
    mods_dir.mkdir(parents=True, exist_ok=True)

    # Clean up any obsolete dr3shskins jars
    for dr3sh_mod in list(mods_dir.glob("*dr3shskins*.jar")) + list(mods_dir.glob("*Dr3shSkins*.jar")):
        try:
            dr3sh_mod.unlink(missing_ok=True)
        except OSError:
            pass

    existing = next(
        (p for p in mods_dir.iterdir() if p.is_file() and p.name.lower().startswith("customskinloader") and p.suffix.lower() == ".jar"),
        None,
    )
    if existing is not None:
        return existing

    for bundled_dir in (paths.assets / "mods", Path(__file__).resolve().parents[1] / "assets" / "mods"):
        if bundled_dir.is_dir():
            bundled_jar = next(
                (p for p in bundled_dir.iterdir() if p.is_file() and p.name.lower().startswith("customskinloader") and p.suffix.lower() == ".jar"),
                None,
            )
            if bundled_jar is not None:
                target_jar = mods_dir / bundled_jar.name
                try:
                    shutil.copyfile(bundled_jar, target_jar)
                    return target_jar
                except OSError:
                    pass

    cache_dir = paths.cache / "mods"
    cache_dir.mkdir(parents=True, exist_ok=True)

    try:
        if status:
            status("Sincronizando mod de skins (CustomSkinLoader)…", None)
        res = requests.get(
            MODRINTH_CSL_VERSIONS_URL,
            headers={"User-Agent": "Dr3shLauncher/1.0"},
            timeout=12,
        )
        if res.ok and res.text:
            selected = _select_csl_mod_asset(res.json(), minecraft_version, loader)
            if selected:
                filename, download_url = selected
                cached_jar = cache_dir / filename
                if not cached_jar.is_file():
                    jar_res = requests.get(download_url, headers={"User-Agent": "Dr3shLauncher/1.0"}, timeout=25)
                    if jar_res.ok and jar_res.content:
                        cached_jar.write_bytes(jar_res.content)
                if cached_jar.is_file():
                    target_jar = mods_dir / filename
                    shutil.copyfile(cached_jar, target_jar)
                    return target_jar
    except Exception:
        pass

    # Offline fallback: if any CustomSkinLoader jar is already in cache, copy it
    cached_fallback = next(
        (p for p in cache_dir.iterdir() if p.is_file() and p.name.lower().startswith("customskinloader") and p.suffix.lower() == ".jar"),
        None,
    )
    if cached_fallback is not None:
        target_jar = mods_dir / cached_fallback.name
        try:
            shutil.copyfile(cached_fallback, target_jar)
            return target_jar
        except OSError:
            return None
    return None