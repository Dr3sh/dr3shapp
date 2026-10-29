"""Paths, constants, settings and library persistence."""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

APP_NAME = "Dr3shLauncher"
APP_VERSION = "0.1.0-prototype"

# NOTE: adjust these two if your versions.json lives in another repo/branch.
DEFAULT_JSON_URL = "https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/refs/heads/main/versions.json"
DEFAULT_ASSETS_BASE = "https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/refs/heads/main/"


def app_root() -> Path:
    """Folder where bundled assets live (works with PyInstaller)."""
    if getattr(sys, "frozen", False):
        return Path(getattr(sys, "_MEIPASS", Path(sys.executable).parent))
    return Path(__file__).resolve().parent.parent


def data_home() -> Path:
    env = os.environ.get("DR3SH_HOME")
    if env:
        return Path(env)
    if os.name == "nt":
        return Path(os.environ.get("APPDATA") or Path.home()) / APP_NAME
    return Path.home() / ".local" / "share" / APP_NAME


def default_minecraft_dir() -> Path:
    if os.name == "nt":
        return Path(os.environ.get("APPDATA") or Path.home()) / ".minecraft"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "minecraft"
    return Path.home() / ".minecraft"


def read_json(path: Path, default):
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def write_json(path: Path, data) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
    os.replace(tmp, path)


class Paths:
    def __init__(self, home: Path | None = None):
        self.home = Path(home) if home else data_home()
        self.config = self.home / "config.json"
        self.library = self.home / "library.json"
        self.cache = self.home / "cache"
        self.images = self.cache / "images"
        self.catalog_cache = self.cache / "versions.json"
        self.etag = self.cache / "versions.etag"
        self.downloads = self.home / "downloads"
        self.instances = self.home / "instances"
        self.shared = self.home / "shared"
        self.backups = self.home / "backups"
        self.logs = self.home / "logs"
        self.state = self.home / "state"
        self.skins = self.home / "skins"
        self.assets = app_root() / "assets"

    def ensure(self) -> None:
        for p in (self.home, self.cache, self.images, self.downloads, self.instances,
                  self.shared, self.backups, self.logs, self.state, self.skins):
            p.mkdir(parents=True, exist_ok=True)


class Settings:
    DEFAULTS = {
        "language": "es",
        "install_mode": "instance",       # "instance" | "default"
        "server_modes": {},               # per-server override
        "instances_dir": "",              # custom base folder for instances
        "min_ram_mb": 1024,
        "ram_mb": 4096,
        "server_ram_mb": {},
        "java_path": "",
        "jvm_args": "",
        "window_width": 854,
        "window_height": 480,
        "fullscreen": False,
        "close_on_launch": "keep",        # "keep" | "minimize" | "close"
        "username": "Player",
        "account_mode": "offline",
        "active_account_id": "",
        "microsoft_client_id": "00000000402b5328",
        "account_username": "Player",
        "account_uuid": "",
        "account_token": "",
        "account_email": "",
        "offline_skin_path": "",
        "offline_skin_model": "classic",  # "classic" (Steve 4px) | "slim" (Alex 3px)
        "custom_skin_mod": True,
        "last_selected_server_id": "",
        "show_intro": True,
        "json_url": DEFAULT_JSON_URL,
        "assets_base_url": DEFAULT_ASSETS_BASE,
    }

    def __init__(self, paths: Paths):
        self.paths = paths
        self.data = dict(self.DEFAULTS)
        stored = read_json(paths.config, {})
        if isinstance(stored, dict):
            self.data.update(stored)

    def __getitem__(self, k):
        return self.data.get(k, self.DEFAULTS.get(k))

    def __setitem__(self, k, v):
        self.data[k] = v

    def save(self) -> None:
        write_json(self.paths.config, self.data)

    def current_account(self) -> dict:
        mode = str(self.data.get("account_mode") or "offline").lower()
        username = (self.data.get("account_username") or self.data.get("username") or "Player").strip() or "Player"
        return {
            "mode": mode if mode in {"offline", "microsoft"} else "offline",
            "username": username,
            "uuid": self.data.get("account_uuid") or "",
            "token": self.data.get("account_token") or "",
            "email": self.data.get("account_email") or "",
        }

    def set_account(self, mode: str, username: str, token: str = "", uuid: str = "", email: str = "") -> None:
        mode = (mode or "offline").lower()
        username = (username or "Player").strip() or "Player"
        self.data["account_mode"] = "microsoft" if mode == "microsoft" else "offline"
        self.data["account_username"] = username
        self.data["username"] = username
        self.data["account_uuid"] = uuid or ""
        self.data["account_token"] = token or ""
        self.data["account_email"] = email or ""
        self.save()

    def server_mode(self, server_id: str) -> str:
        return self.data.get("server_modes", {}).get(server_id) or self["install_mode"]

    def set_server_mode(self, server_id: str, mode: str) -> None:
        self.data.setdefault("server_modes", {})[server_id] = mode
        self.save()

    def instances_base(self) -> Path:
        custom = (self["instances_dir"] or "").strip()
        return Path(custom) if custom else self.paths.instances

    def game_dir(self, server_id: str, mode: str) -> Path:
        if mode == "default":
            return default_minecraft_dir()
        return self.instances_base() / server_id


class Library:
    """Servers the player has 'obtained' from the Marketplace."""

    def __init__(self, paths: Paths):
        self.paths = paths
        ids = read_json(paths.library, [])
        self.ids: list[str] = [i for i in ids if isinstance(i, str)] if isinstance(ids, list) else []

    def has(self, server_id: str) -> bool:
        return server_id in self.ids

    def add(self, server_id: str) -> None:
        if server_id not in self.ids:
            self.ids.append(server_id)
            write_json(self.paths.library, self.ids)

    def remove(self, server_id: str) -> None:
        if server_id in self.ids:
            self.ids.remove(server_id)
            write_json(self.paths.library, self.ids)
