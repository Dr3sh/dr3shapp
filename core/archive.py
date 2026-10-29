"""Extract .zip (zipfile) and .rar (bundled / installed 7-Zip) archives safely."""
from __future__ import annotations

import os
import shutil
import subprocess
import zipfile
from pathlib import Path

from .config import app_root


class ArchiveError(Exception):
    pass


def find_7z() -> str | None:
    """Bundled 7-Zip first (assets/bin), then PATH, then the default Windows install."""
    bin_dir = app_root() / "assets" / "bin"
    for name in ("7z.exe", "7za.exe", "7zr.exe", "7z", "7za", "7zz"):
        p = bin_dir / name
        if p.exists():
            return str(p)
    for name in ("7z", "7za", "7zr", "7zz"):
        found = shutil.which(name)
        if found:
            return found
    for p in (r"C:\Program Files\7-Zip\7z.exe", r"C:\Program Files (x86)\7-Zip\7z.exe"):
        if os.path.exists(p):
            return p
    return None


def kind_of(path: Path) -> str:
    lower = path.name.lower()
    if lower.endswith(".zip"):
        return "zip"
    if lower.endswith((".rar", ".7z")):
        return "rar" if lower.endswith(".rar") else "rar"
    try:
        with open(path, "rb") as f:
            magic = f.read(8)
    except OSError as exc:
        raise ArchiveError(f"Could not read archive: {path}") from exc
    if magic.startswith(b"PK"):
        return "zip"
    if magic.startswith(b"Rar!") or magic.startswith(b"7z\xbc\xaf\x27\x1c"):
        return "rar"
    raise ArchiveError("Unknown archive format (expected .zip or .rar)")


def _extract_zip(path: Path, dest: Path) -> None:
    base = dest.resolve()
    with zipfile.ZipFile(path) as zf:
        for member in zf.infolist():
            target = (dest / member.filename).resolve()
            if target != base and base not in target.parents:
                raise ArchiveError(f"Unsafe path in archive: {member.filename}")
        zf.extractall(dest)


def _extract_with_7z(path: Path, dest: Path) -> None:
    exe = find_7z()
    if not exe:
        raise ArchiveError("7-Zip not found. Put 7z.exe + 7z.dll in assets/bin/ (needed for .rar)")
    proc = subprocess.run([exe, "x", "-y", f"-o{dest}", str(path)],
                          capture_output=True, text=True,
                          creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    if proc.returncode not in (0, 1):                  # 1 = warnings only
        raise ArchiveError(f"7-Zip failed: {(proc.stderr or proc.stdout).strip()[-300:]}")


GAME_ROOT_NAMES = {
    "config", "mods", "resourcepacks", "shaderpacks", "scripts",
    "defaultconfigs", "kubejs", "packmenu", "overrides", "libraries",
}

# Files that archivers/OSes add and that must never be installed.
JUNK_NAMES = {".ds_store", "thumbs.db", "desktop.ini"}
JUNK_DIRS = {"__macosx"}


def is_junk(member: str) -> bool:
    parts = member.replace("\\", "/").strip("/").split("/")
    return parts[-1].lower() in JUNK_NAMES or any(p.lower() in JUNK_DIRS for p in parts)


def strip_wrapper(root: Path) -> Path:
    """Only strip a wrapper folder when it is an unnecessary archive container.

    Archive containers are skipped until the package root is reached. A lone
    game directory such as ``mods`` or ``config`` is content, not a container.
    """
    while True:
        try:
            entries = [e for e in root.iterdir() if not is_junk(e.name)]
        except OSError:
            return root

        if len(entries) == 1 and entries[0].is_dir():
            candidate = entries[0]
            if candidate.name.lower() not in GAME_ROOT_NAMES:
                root = candidate
                continue
        return root


def detect_wrapper_prefix(members: list[str]) -> str | None:
    """Same rule as :func:`strip_wrapper`, but for a list of archive file paths.

    Returns e.g. ``"MyPack/1.0"`` when every file lives under that container, so a
    manifest built from the archive installs ``mods/x.jar`` and not ``MyPack/1.0/mods/x.jar``.
    """
    paths = [m.replace("\\", "/").strip("/").split("/") for m in members if m and not is_junk(m)]
    prefix: list[str] = []
    while paths:
        heads = {p[0] for p in paths}
        if len(heads) != 1 or any(len(p) < 2 for p in paths):
            break
        head = next(iter(heads))
        if head.lower() in GAME_ROOT_NAMES:
            break
        prefix.append(head)
        paths = [p[1:] for p in paths]
    return "/".join(prefix) or None


def extract(archive: Path, dest: Path) -> Path:
    """Extract into `dest` and return the real package root (wrapper folder stripped)."""
    if dest.exists():
        shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True, exist_ok=True)
    kind = kind_of(archive)
    if kind == "zip":
        _extract_zip(archive, dest)
    else:
        _extract_with_7z(archive, dest)
    return strip_wrapper(dest)
