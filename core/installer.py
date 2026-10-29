"""Managed-files installer.

The launcher only ever touches files it installed itself (tracked per server in
state/<server_id>.json). Player data (saves, screenshots, options.txt, ...) is never touched.

How an update decides what to do with every file (Steam-style, hash based):

* ``protected``  saves/, screenshots/, map waypoints... -> never read, written or deleted.
* ``seed``       options.txt, servers.dat, ...          -> installed only if missing, then owned
                                                           by the player (never overwritten/removed).
* ``config``     config/**                              -> if the player (or the mod itself) changed
                                                           the file and the pack did NOT change it,
                                                           the local version is kept. If the pack
                                                           changed it too, the pack wins and the old
                                                           local copy is saved in backups/.
* ``managed``    mods/, kubejs/, resourcepacks/ ...     -> always forced to match the pack exactly.

Files that the pack used to ship and no longer ships are removed (complete manifests).
Files the player added (never installed by the launcher) are never removed.
"""
from __future__ import annotations

import os
import re
import shutil
import time
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

from .archive import is_junk
from .config import Paths, read_json, write_json
from .downloader import sha256_file

STAGING_DIR = ".dr3sh-staging"

PROTECTED_DIRS = {
    "saves", "screenshots", "logs", "crash-reports", "backups",
    "xaerowaypoints", "xaeroworldmap", "schematics", "replay_recordings", "replay_videos",
    STAGING_DIR,
}
PROTECTED_PREFIXES = ("journeymap/data/",)
PROTECTED_FILES = {"servers.dat_old", "usercache.json", "usernamecache.json",
                   "launcher_profiles.json", "launcher_accounts.json",
                   "hotbar.nbt", "command_history.txt", "realms_persistence.json"}
# Shipped as defaults by the pack, but owned by the player once they exist.
SEED_FILES = {"options.txt", "optionsof.txt", "optionsshaders.txt", "servers.dat"}
# Player/mod edits are preserved here unless the pack itself changes the same file.
USER_CONFIG_PREFIXES = ("config/",)
POLICIES = ("seed", "config", "managed")
BACKUP_KEEP = 3


class UnsafePath(Exception):
    pass


class GameRunningError(Exception):
    pass


def safe_rel(rel: str) -> str:
    """Normalize a relative path from a package/manifest or reject it."""
    rel = (rel or "").replace("\\", "/").strip()
    if not rel or rel.startswith("/") or (len(rel) > 1 and rel[1] == ":"):
        raise UnsafePath(f"Absolute or empty path not allowed: {rel!r}")
    parts = [p for p in rel.split("/") if p not in ("", ".")]
    if not parts or any(p == ".." for p in parts):
        raise UnsafePath(f"Path traversal not allowed: {rel!r}")
    return "/".join(parts)


def safe_join(base: Path, rel: str) -> Path:
    rel = safe_rel(rel)
    base_r = base.resolve()
    target = (base / rel).resolve()
    if base_r not in target.parents:
        raise UnsafePath(f"Path escapes the game folder: {rel!r}")
    return target


def is_protected(rel: str) -> bool:
    low = safe_rel(rel).lower()
    top = low.split("/")[0]
    return top in PROTECTED_DIRS or low in PROTECTED_FILES or low.startswith(PROTECTED_PREFIXES)


def is_seed(rel: str) -> bool:
    return safe_rel(rel).lower() in SEED_FILES


def is_user_config(rel: str) -> bool:
    return safe_rel(rel).lower().startswith(USER_CONFIG_PREFIXES)


def file_policy(rel: str, entry: dict | None = None) -> str:
    """'protected' | 'seed' | 'config' | 'managed' (a manifest entry may override, except protected)."""
    rel = safe_rel(rel)
    if is_protected(rel):
        return "protected"
    override = str((entry or {}).get("policy") or "").strip().lower()
    if override in POLICIES:
        return override
    if is_seed(rel):
        return "seed"
    if is_user_config(rel):
        return "config"
    return "managed"


def normalize_version_key(value: str | None) -> str:
    if value is None:
        return ""
    return re.sub(r"[^a-z0-9]+", "", str(value).strip().lower())


def version_matches_reference(stored_value: str | None, version_id: str | None = None, version_title: str | None = None) -> bool:
    if not stored_value:
        return False
    candidate_values = [stored_value]
    if version_id:
        candidate_values.append(version_id)
    if version_title:
        candidate_values.append(version_title)
    normalized = [normalize_version_key(value) for value in candidate_values if value is not None]
    return bool(normalized) and any(normalized[0] == other for other in normalized[1:])


# --------------------------------------------------------------------- state
class StateStore:
    """Per-server install state: version, mode, game dir and managed files with hashes."""

    def __init__(self, paths: Paths):
        self.dir = paths.state

    def _p(self, server_id: str) -> Path:
        return self.dir / f"{server_id}.json"

    def get(self, server_id: str) -> dict | None:
        data = read_json(self._p(server_id), None)
        return data if isinstance(data, dict) and data.get("version_id") else None

    def save(self, server_id: str, state: dict) -> None:
        write_json(self._p(server_id), state)

    def delete(self, server_id: str) -> None:
        self._p(server_id).unlink(missing_ok=True)

    def all(self) -> dict[str, dict]:
        out = {}
        if self.dir.exists():
            for p in self.dir.glob("*.json"):
                data = read_json(p, None)
                if isinstance(data, dict) and data.get("version_id"):
                    out[p.stem] = data
        return out


def new_state(server_id: str, version_id: str, mode: str, game_dir: Path, files: dict,
              release_version: str = "", file_cache: dict | None = None) -> dict:
    return {"server_id": server_id, "version_id": version_id, "release_version": release_version, "mode": mode,
            "game_dir": str(game_dir), "files": files, "file_cache": file_cache or {},
            "installed_at": time.strftime("%Y-%m-%d %H:%M:%S")}


def is_installation_complete(state: dict, expected_game_dir: Path) -> bool:
    """Check tracked files without hashing them, so startup verification stays cheap."""
    if not isinstance(state, dict) or not isinstance(state.get("files"), dict):
        return False
    try:
        game_dir = Path(state.get("game_dir", "")).resolve()
        expected = Path(expected_game_dir).resolve()
    except (OSError, RuntimeError, ValueError):
        return False
    if game_dir != expected or not expected.is_dir():
        return False
    for rel in state["files"]:
        try:
            if not safe_join(expected, rel).is_file():
                return False
        except (UnsafePath, OSError, RuntimeError, ValueError):
            return False
    return True


# ------------------------------------------------------------------- helpers
def _prune_empty_parents(path: Path, stop: Path) -> None:
    stop = stop.resolve()
    parent = path.parent
    while parent != stop and stop in parent.parents and parent.exists() and not any(parent.iterdir()):
        parent.rmdir()
        parent = parent.parent


def remove_managed(game_dir: Path, files: dict | list) -> int:
    """Delete managed files (never protected/seed ones) and prune empty folders."""
    removed = 0
    for rel in list(files):
        try:
            if is_protected(rel) or is_seed(rel):
                continue
            p = safe_join(game_dir, rel)
        except UnsafePath:
            continue
        if p.is_file():
            p.unlink()
            removed += 1
            _prune_empty_parents(p, game_dir)
    return removed


def _rotate_backups(backups_dir: Path, label: str) -> None:
    old = sorted(backups_dir.glob(f"{label}-*.zip"))
    for stale in old[:-BACKUP_KEEP]:
        stale.unlink(missing_ok=True)


def backup_files(game_dir: Path, rels: list[str], backups_dir: Path, label: str) -> Path | None:
    """Zip the managed files that exist (kept for manual recovery)."""
    existing = []
    for rel in rels:
        try:
            p = safe_join(game_dir, rel)
        except UnsafePath:
            continue
        if p.is_file():
            existing.append((rel, p))
    if not existing:
        return None
    backups_dir.mkdir(parents=True, exist_ok=True)
    out = backups_dir / f"{label}-{time.strftime('%Y%m%d-%H%M%S')}.zip"
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
        for rel, p in existing:
            zf.write(p, rel)
    _rotate_backups(backups_dir, label)
    return out


def restore_backup(game_dir: Path, backup: Path | None) -> None:
    if backup and backup.exists():
        with zipfile.ZipFile(backup) as zf:
            for name in zf.namelist():
                target = safe_join(game_dir, name)
                target.parent.mkdir(parents=True, exist_ok=True)
                with zf.open(name) as src, open(target, "wb") as dst:
                    shutil.copyfileobj(src, dst)


def _walk_package(root: Path) -> dict[str, Path]:
    found = {}
    for p in root.rglob("*"):
        if p.is_file():
            rel = safe_rel(p.relative_to(root).as_posix())
            if not is_protected(rel) and not is_junk(rel):
                found[rel] = p
    return found


def _stat_key(path: Path) -> list[int]:
    st = path.stat()
    return [int(st.st_size), int(st.st_mtime_ns)]


class HashCache:
    """sha256 of local files, reusing the previous result when size+mtime did not change."""

    def __init__(self, cached: dict | None = None, full_check: bool = False):
        self.cached = cached if isinstance(cached, dict) else {}
        self.full_check = full_check
        self.result: dict[str, list] = {}

    def hash(self, rel: str, path: Path) -> str | None:
        if not path.is_file():
            return None
        key = _stat_key(path)
        prev = self.cached.get(rel)
        if (not self.full_check and isinstance(prev, list) and len(prev) == 3
                and prev[:2] == key and isinstance(prev[2], str)):
            digest = prev[2]
        else:
            digest = sha256_file(path)
        self.result[rel] = key + [digest]
        return digest


Progress = Callable[[float], None]
FetchFile = Callable[[str, Path], None]             # (url, dest) -> downloads the file
FetchManifestEntry = Callable[[dict, Path], None]   # (entry, dest) -> gets the file from a shared source


# ------------------------------------------------------------------ planning
@dataclass
class UpdatePlan:
    downloads: list[dict] = field(default_factory=list)   # entries to fetch and write
    removals: list[str] = field(default_factory=list)     # tracked files to delete
    keep: dict[str, str] = field(default_factory=dict)    # already correct (or preserved) -> pack hash
    preserved: list[str] = field(default_factory=list)    # local config edits kept on purpose
    overwritten: list[str] = field(default_factory=list)  # local edits replaced by the pack (backed up)
    seeds: list[str] = field(default_factory=list)        # seed files installed because missing
    complete: bool = False
    files: dict[str, str] = field(default_factory=dict)   # tracked map after a successful apply
    hash_cache: dict[str, list] = field(default_factory=dict)

    @property
    def download_bytes(self) -> int:
        return sum(int(e.get("size") or 0) for e in self.downloads)

    @property
    def has_changes(self) -> bool:
        return bool(self.downloads or self.removals)

    def summary(self) -> dict:
        return {"download": len(self.downloads), "remove": len(self.removals), "keep": len(self.keep),
                "preserved": list(self.preserved), "overwritten": list(self.overwritten),
                "download_bytes": self.download_bytes}


def plan_update(manifest: dict, game_dir: Path, prev_files: dict, prev_cache: dict | None = None,
                full_check: bool = False) -> UpdatePlan:
    """Compare the pack index with what is on disk and decide, file by file, what to do."""
    if not isinstance(manifest, dict) or not isinstance(manifest.get("files") or [], list):
        raise ValueError("Invalid manifest: 'files' must be a list")
    plan = UpdatePlan(complete=manifest.get("complete") is True)
    prev: dict[str, str] = {}
    for rel, digest in (prev_files or {}).items():
        try:
            prev[safe_rel(rel)] = str(digest or "").lower()
        except UnsafePath:
            continue

    wanted: dict[str, dict] = {}
    for e in manifest.get("files") or []:
        if not isinstance(e, dict):
            raise ValueError("Invalid manifest entry")
        rel = safe_rel(e.get("path", ""))
        policy = file_policy(rel, e)
        if policy == "protected":
            continue
        wanted[rel] = {**e, "path": rel, "sha256": str(e.get("sha256") or "").lower(), "policy": policy}

    cache = HashCache(prev_cache, full_check)
    for rel, e in sorted(wanted.items()):
        dst = safe_join(game_dir, rel)
        new = e["sha256"]
        local = cache.hash(rel, dst)
        old = prev.get(rel) or None
        policy = e["policy"]

        if policy == "seed":
            if local is None:
                plan.downloads.append(e)
                plan.seeds.append(rel)
            continue                                   # never tracked -> never removed
        if local is not None and new and local == new:
            plan.keep[rel] = new
            continue
        if local is None:
            plan.downloads.append(e)
            continue
        locally_changed = old is None or local != old
        if locally_changed or policy == "config":
            plan.overwritten.append(rel)
        plan.downloads.append(e)

    # Any config/ file (or child folder) from previous versions that is omitted in the
    # new update means "no changes" -> preserve it on disk and never delete it.
    for r, old_hash in sorted(prev.items()):
        if r not in wanted and is_user_config(r):
            plan.keep[r] = old_hash
            plan.preserved.append(r)

    removals = set()
    for r in manifest.get("remove") or []:
        removals.add(safe_rel(r))
    if plan.complete:
        removals.update(r for r in prev if r not in wanted)
    plan.removals = sorted(r for r in removals
                           if r not in wanted and not is_protected(r) and not is_seed(r) and not is_user_config(r))

    tracked = {rel: e["sha256"] for rel, e in wanted.items() if e["policy"] != "seed"}
    if plan.complete:
        plan.files = {r: h for r, h in prev.items() if r not in wanted and is_user_config(r)}
        plan.files.update(tracked)
    else:
        plan.files = {r: h for r, h in prev.items() if r not in plan.removals}
        plan.files.update(tracked)
    plan.hash_cache = cache.result
    return plan


def plan_manifest(manifest: dict, game_dir: Path, prev_files: dict) -> tuple[list[dict], list[str]]:
    """Return changed files and removals (kept for backwards compatibility)."""
    plan = plan_update(manifest, game_dir, prev_files)
    return plan.downloads, plan.removals


# ------------------------------------------------------------------- apply
@dataclass
class ApplyResult:
    files: dict[str, str]
    file_cache: dict[str, list]
    backup: Path | None = None


def _is_lock_error(exc: BaseException) -> bool:
    return isinstance(exc, PermissionError) or getattr(exc, "winerror", None) in (5, 32, 33)


def apply_plan(plan: UpdatePlan, game_dir: Path, backups_dir: Path, label: str,
               fetch_entry: FetchManifestEntry | None, progress: Progress | None = None) -> ApplyResult:
    """Fetch + verify every file into a staging folder, then swap atomically. Rolls back on failure.

    Old files are *moved* aside (instant on the same disk) instead of being zipped, so
    rolling back a 1 GB update costs nothing. Only player-edited files that the pack
    overwrites are copied into backups/ for manual recovery.
    """
    game_dir.mkdir(parents=True, exist_ok=True)
    staging = game_dir / STAGING_DIR
    shutil.rmtree(staging, ignore_errors=True)
    new_dir, old_dir = staging / "new", staging / "old"
    new_dir.mkdir(parents=True, exist_ok=True)
    old_dir.mkdir(parents=True, exist_ok=True)
    created: list[Path] = []
    moved: list[tuple[Path, Path]] = []
    backup: Path | None = None
    try:
        staged: list[tuple[dict, Path]] = []
        total = max(len(plan.downloads), 1)
        for i, e in enumerate(plan.downloads, 1):
            if fetch_entry is None:
                raise ValueError(f"No download source configured for {e['path']}")
            tmp = new_dir / f"{i}.bin"
            sha = e.get("sha256") or ""
            for _attempt in range(3):
                tmp.unlink(missing_ok=True)
                fetch_entry(e, tmp)
                if not sha or sha256_file(tmp) == sha:
                    break
            else:
                tmp.unlink(missing_ok=True)
                raise IOError(f"Checksum mismatch for {e['path']}")
            staged.append((e, tmp))
            if progress:
                progress(i / total * 0.9)

        def move_aside(dst: Path, rel: str) -> None:
            saved = old_dir / rel
            saved.parent.mkdir(parents=True, exist_ok=True)
            os.replace(dst, saved)
            moved.append((dst, saved))

        for rel in plan.removals:
            p = safe_join(game_dir, rel)
            if p.is_file():
                move_aside(p, rel)
        for e, tmp in staged:
            dst = safe_join(game_dir, e["path"])
            if dst.is_file():
                move_aside(dst, e["path"])
            else:
                created.append(dst)
            dst.parent.mkdir(parents=True, exist_ok=True)
            os.replace(tmp, dst)
    except BaseException as exc:
        for p in created:
            try:
                p.unlink(missing_ok=True)
            except OSError:
                pass
        for dst, saved in reversed(moved):
            try:
                dst.parent.mkdir(parents=True, exist_ok=True)
                os.replace(saved, dst)
            except OSError:
                pass
        shutil.rmtree(staging, ignore_errors=True)
        if _is_lock_error(exc):
            raise GameRunningError(
                "Hay archivos del modpack en uso. Cierra Minecraft y vuelve a intentarlo.") from exc
        raise

    # Success: keep a copy of player-edited files that the pack replaced or removed.
    keep_copies = [rel for rel in plan.overwritten + plan.removals if (old_dir / rel).is_file()
                   and file_policy(rel) == "config"]
    if keep_copies:
        try:
            backups_dir.mkdir(parents=True, exist_ok=True)
            backup = backups_dir / f"{label}-{time.strftime('%Y%m%d-%H%M%S')}.zip"
            with zipfile.ZipFile(backup, "w", zipfile.ZIP_DEFLATED) as zf:
                for rel in keep_copies:
                    zf.write(old_dir / rel, rel)
            _rotate_backups(backups_dir, label)
        except OSError:
            backup = None
    for rel in plan.removals:
        try:
            _prune_empty_parents(safe_join(game_dir, rel), game_dir)
        except (UnsafePath, OSError):
            pass
    shutil.rmtree(staging, ignore_errors=True)

    files = dict(plan.files)
    for e in plan.downloads:                          # entries without sha256 (legacy manifests)
        if e["path"] in files and not files[e["path"]]:
            files[e["path"]] = sha256_file(safe_join(game_dir, e["path"]))
    downloaded = {e["path"] for e in plan.downloads}
    cache: dict[str, list] = {}
    for rel in files:
        try:
            p = safe_join(game_dir, rel)
            if not p.is_file():
                continue
            key = _stat_key(p)
        except (UnsafePath, OSError):
            continue
        known = plan.hash_cache.get(rel)
        if known and known[:2] == key:
            cache[rel] = known
        elif rel in downloaded:
            cache[rel] = key + [files[rel]]
    if progress:
        progress(1.0)
    return ApplyResult(files=files, file_cache=cache, backup=backup)


# --------------------------------------------------------- Mode A: full package
def package_manifest(root: Path, prev_files: dict) -> tuple[dict, dict[str, Path]]:
    """Build a manifest from an extracted package (partial semantics, scoped removals)."""
    package = _walk_package(root)
    touched_roots = {rel.split("/", 1)[0] for rel in package}
    entries = [{"path": rel, "sha256": sha256_file(src), "size": src.stat().st_size}
               for rel, src in sorted(package.items())]
    removals = []
    for r in prev_files or {}:
        try:
            rel = safe_rel(r)
        except UnsafePath:
            continue
        if rel not in package and rel.split("/", 1)[0] in touched_roots and not is_user_config(rel):
            removals.append(rel)
    return {"complete": False, "files": entries, "remove": removals}, package


def apply_package(root: Path, game_dir: Path, prev_files: dict, backups_dir: Path, label: str,
                  progress: Progress | None = None, prev_cache: dict | None = None,
                  full_check: bool = False, result: list | None = None) -> dict:
    """Apply an extracted package. Returns the new {rel: sha256} managed-files map.

    A partial update should only prune files in the same root namespace as the
    package being applied. So a mods-only archive may remove stale mods while
    preserving unrelated directories such as config/, resourcepacks/ and saves/.
    """
    manifest, package = package_manifest(root, prev_files)
    plan = plan_update(manifest, game_dir, prev_files, prev_cache, full_check)

    def fetch_entry(entry: dict, dest: Path) -> None:
        shutil.copy2(package[entry["path"]], dest)

    applied = apply_plan(plan, game_dir, backups_dir, label, fetch_entry, progress)
    applied.files = {rel: applied.files[rel] for rel in package if rel in applied.files}
    if result is not None:
        result.extend([applied, plan])
    return applied.files


# ------------------------------------------------------- Mode B: delta manifest
def apply_manifest(manifest: dict, game_dir: Path, prev_files: dict, backups_dir: Path, label: str,
                   fetch: FetchFile | None, progress: Progress | None = None,
                   fetch_entry: FetchManifestEntry | None = None) -> dict:
    """Download only what changed, verify every hash, then swap files in. Rolls back on failure."""
    plan = plan_update(manifest, game_dir, prev_files)

    def get(entry: dict, dest: Path) -> None:
        if fetch_entry:
            fetch_entry(entry, dest)
        elif fetch and entry.get("url"):
            fetch(entry["url"], dest)
        else:
            raise ValueError(f"No download source configured for {entry['path']}")

    return apply_plan(plan, game_dir, backups_dir, label, get, progress).files


# ----------------------------------------------------- switching / uninstalling
def release_game_dir(store: StateStore, game_dir: Path, except_server: str) -> list[str]:
    """'Default folder' mode: before installing a pack, remove other packs' managed files
    from the same game folder so only one modpack lives there at a time."""
    released = []
    target = game_dir.resolve()
    for sid, st in store.all().items():
        if sid == except_server:
            continue
        if Path(st.get("game_dir", "")).resolve() == target:
            remove_managed(target, st.get("files", {}))
            store.delete(sid)
            released.append(sid)
    return released


def uninstall(store: StateStore, server_id: str) -> None:
    st = store.get(server_id)
    if st:
        remove_managed(Path(st["game_dir"]), st.get("files", {}))
        store.delete(server_id)
