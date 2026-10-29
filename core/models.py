"""Catalog models + the visibility / latest-version rules of the project spec."""
from __future__ import annotations

import re
from dataclasses import dataclass, field


class CatalogError(Exception):
    pass


def is_visible(status) -> bool:
    return str(status or "active").strip().lower() != "hidden"


@dataclass
class Server:
    id: str
    title: str
    description: str = ""
    image: str = ""
    logo: str = ""
    logo_square: str = ""
    theme_color: str = ""
    status: str = "active"
    minecraft_version: str = ""
    mod_loader: str = ""
    mod_loader_version: str = ""
    screenshots: list = field(default_factory=list)
    server_address: str = ""


@dataclass
class Version:
    id: str
    server: str
    title: str
    version: str = ""
    link: str = ""
    description: str = ""
    published_at: str = ""
    size_mb: float | None = None
    status: str = "active"
    minecraft_version: str = ""
    mod_loader: str = ""
    mod_loader_version: str = ""
    theme_color: str = ""
    sha256: str = ""
    manifest: str = ""
    min_ram_mb: int | None = None
    order: int = 0   # position in the JSON array (tie-break for equal dates)


def version_sort_key(value: str) -> tuple:
    """Return a sortable tuple that keeps semantic version-ish IDs ordered.

    Examples: v2 > v1, v1.0.1 > v1.0.0, v2.0-beta < v2.0.
    """
    text = str(value or "").strip().lower()
    digits = [int(part) for part in re.findall(r"\d+", text)]
    suffix = re.search(r"(alpha|beta|rc|preview|test|dev|snapshot)", text)
    suffix_value = {
        "dev": -3,
        "snapshot": -2,
        "alpha": -2,
        "beta": -1,
        "preview": -1,
        "test": -1,
        "rc": 0,
    }.get(suffix.group(1) if suffix else "", 1)
    pad = [0] * max(3 - len(digits), 0)
    return tuple(digits + pad + [suffix_value])


def compare_version_strings(left: str | None, right: str | None) -> int:
    left_key = version_sort_key(left or "")
    right_key = version_sort_key(right or "")
    if left_key < right_key:
        return -1
    if left_key > right_key:
        return 1
    return 0


class Catalog:
    def __init__(self, servers: dict[str, Server], versions: dict[str, list[Version]]):
        self.servers = servers          # visible servers only
        self.versions = versions        # server_id -> newest first

    def latest(self, server_id: str) -> Version | None:
        vs = self.versions.get(server_id) or []
        if not vs:
            return None
        return vs[0]

    def version(self, server_id: str, version_id: str) -> Version | None:
        for v in self.versions.get(server_id, []):
            if v.id == version_id:
                return v
        return None

    def meta(self, server: Server, version: Version | None) -> dict:
        """Minecraft version / loader: the version value overrides the server value."""
        def pick(attr):
            return (getattr(version, attr, "") if version else "") or getattr(server, attr, "") or ""
        return {k: pick(k) for k in ("minecraft_version", "mod_loader", "mod_loader_version")}

    def theme_color(self, server: Server, version: Version | None = None) -> str:
        return (version.theme_color if version else "") or server.theme_color


def _s(d: dict, key: str, default: str = "") -> str:
    v = d.get(key, default)
    return default if v is None else str(v)


def _theme_color(d: dict) -> str:
    color = _s(d, "theme_color", _s(d, "color")).strip()
    if len(color) == 7 and color.startswith("#") and all(c in "0123456789abcdefABCDEF" for c in color[1:]):
        return color.lower()
    return ""


def parse_catalog(data) -> Catalog:
    if not isinstance(data, dict) or not isinstance(data.get("servers"), list) \
            or not isinstance(data.get("versions"), list):
        raise CatalogError("versions.json must contain 'servers' and 'versions' lists")

    servers: dict[str, Server] = {}
    for raw in data["servers"]:
        if not isinstance(raw, dict) or not raw.get("id"):
            continue
        if not is_visible(raw.get("status")):
            continue                                   # hidden => invisible everywhere
        shots = raw.get("screenshots") or []
        servers[str(raw["id"])] = Server(
            id=str(raw["id"]),
            title=_s(raw, "title", str(raw["id"])),
            description=_s(raw, "description"),
            image=_s(raw, "image"),
            logo=_s(raw, "logo"),
            logo_square=_s(raw, "logo_square"),
            theme_color=_theme_color(raw),
            status=_s(raw, "status", "active"),
            minecraft_version=_s(raw, "minecraft_version"),
            mod_loader=_s(raw, "mod_loader"),
            mod_loader_version=_s(raw, "mod_loader_version"),
            screenshots=[dict(x) if isinstance(x, dict) else str(x) for x in shots] if isinstance(shots, list) else [],
            server_address=_s(raw, "server_address"),
        )

    versions: dict[str, list[Version]] = {sid: [] for sid in servers}
    for i, raw in enumerate(data["versions"]):
        if not isinstance(raw, dict) or not raw.get("id"):
            continue
        sid = _s(raw, "server")
        if sid not in servers or not is_visible(raw.get("status")):
            continue
        size = raw.get("size_mb")
        ram = raw.get("min_ram_mb")
        versions[sid].append(Version(
            id=str(raw["id"]), server=sid, title=_s(raw, "title", str(raw["id"])),
            version=_s(raw, "version"),
            link=_s(raw, "link"), description=_s(raw, "description"),
            published_at=_s(raw, "published_at"),
            size_mb=float(size) if isinstance(size, (int, float)) else None,
            status=_s(raw, "status", "active"),
            minecraft_version=_s(raw, "minecraft_version"),
            mod_loader=_s(raw, "mod_loader"),
            mod_loader_version=_s(raw, "mod_loader_version"),
            theme_color=_theme_color(raw),
            sha256=_s(raw, "sha256").lower(), manifest=_s(raw, "manifest"),
            min_ram_mb=int(ram) if isinstance(ram, (int, float)) else None,
            order=i,
        ))
    for vs in versions.values():
        vs.sort(key=lambda v: (
            version_sort_key(v.version or v.id),
            version_sort_key(v.id),
            version_sort_key(v.title),
            v.published_at,
            v.order,
        ), reverse=True)
    return Catalog(servers, versions)
