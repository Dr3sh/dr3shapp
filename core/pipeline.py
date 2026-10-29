"""Install pipeline: manifest -> plan -> fetch only what changed -> verify -> atomic apply.

Update flow for a version that has a manifest (what the developer menu publishes):

1. Read the manifest (complete index of the pack: path + sha256 + size per file).
2. Plan: compare every file with the local state (cheap: size+mtime cache, sha256 when needed)
   and decide keep / download / preserve player config / seed / remove.
3. Fetch only the files that differ. They are read straight out of the release ZIP with
   HTTP Range requests (a few MB instead of the whole pack). When ranges are not
   supported, or most of the pack changed anyway, the full ZIP is downloaded once.
4. Verify each file's sha256, then swap everything in atomically (rollback on failure).

Versions without a manifest still work with the legacy "full package" mode.
"""
from __future__ import annotations

import shutil
import stat
import threading
import time
import zipfile
from pathlib import Path
from typing import Callable

import requests

from . import installer
from .archive import extract
from .config import Paths, Settings
from .data_source import DataSource
from .downloader import download_file
from .models import Server, Version
from .remote_zip import RangeNotSupported, RemoteZip, RemoteZipError
from .resolvers import ResolveError, resolve

# progress(stage, fraction or None, extra text)
ProgressFn = Callable[[str, float | None, str], None]

# If the changed files are more than this share of the ZIP, a single full download is faster.
FULL_DOWNLOAD_RATIO = 0.7


class DiskSpaceError(Exception):
    pass


def _fmt_speed(bps: float) -> str:
    return f"{bps / (1024 * 1024):.1f} MB/s" if bps >= 1024 * 1024 else f"{bps / 1024:.0f} KB/s"


def _fmt_size(num: float) -> str:
    if num >= 1024 ** 3:
        return f"{num / 1024 ** 3:.2f} GB"
    if num >= 1024 ** 2:
        return f"{num / 1024 ** 2:.1f} MB"
    return f"{num / 1024:.0f} KB"


def _check_disk(path: Path, size_mb: float | None, factor: float = 2.5) -> None:
    if not size_mb:
        return
    probe = path
    while not probe.exists() and probe != probe.parent:
        probe = probe.parent
    free = shutil.disk_usage(probe).free
    need = int(size_mb * 1024 * 1024 * factor)
    if free < need:
        raise DiskSpaceError(
            f"Not enough disk space: need ~{need // (1024 ** 2)} MB, free {free // (1024 ** 2)} MB")


def get_manifest_archive_url(manifest: dict, version: Version) -> str:
    """Use an explicit manifest URL, or reuse the version's package link for ZIP-backed entries."""
    explicit_url = str(manifest.get("archive_url") or "").strip()
    if explicit_url:
        return explicit_url
    if any(not entry.get("url") for entry in (manifest.get("files") or [])):
        return str(version.link or "").strip()
    return ""


# ------------------------------------------------------------ archive sources
class ZipSource:
    """Extract manifest entries from an open ZipFile (local or remote)."""

    def __init__(self, zf: zipfile.ZipFile, cleanup: Callable[[], None], mode: str):
        self.zip = zf
        self._cleanup = cleanup
        self.mode = mode                      # "range" | "full"
        self.members: dict[str, zipfile.ZipInfo] = {}
        for info in zf.infolist():
            if info.is_dir():
                continue
            member_path = installer.safe_rel(info.filename)
            if stat.S_ISLNK(info.external_attr >> 16):
                raise ValueError(f"El ZIP contiene un enlace simbólico: {member_path}")
            if member_path in self.members:
                raise ValueError(f"El ZIP contiene una ruta duplicada: {member_path}")
            self.members[member_path] = info

    def info_for(self, entry: dict) -> zipfile.ZipInfo:
        member_path = installer.safe_rel(entry.get("archive_path") or entry["path"])
        info = self.members.get(member_path)
        if info is None:
            raise FileNotFoundError(f"No se encontró {member_path} dentro del ZIP de actualización")
        return info

    def extract(self, entry: dict, dest: Path) -> None:
        with self.zip.open(self.info_for(entry)) as source, dest.open("wb") as target:
            shutil.copyfileobj(source, target, 1024 * 1024)

    def close(self) -> None:
        try:
            self.zip.close()
        finally:
            self._cleanup()


class _ByteProgress:
    def __init__(self, total: int, progress: ProgressFn, label: str):
        self.total = max(total, 1)
        self.progress = progress
        self.label = label
        self.done = 0
        self._t = time.monotonic()
        self._last_done = 0
        self.speed = 0.0

    def __call__(self, n: int) -> None:
        self.done += n
        now = time.monotonic()
        if now - self._t >= 0.25:
            self.speed = (self.done - self._last_done) / (now - self._t)
            self._t, self._last_done = now, self.done
            self.progress("downloading", min(self.done / self.total, 1.0),
                          f"{self.label}{_fmt_size(self.done)} / {_fmt_size(self.total)} · {_fmt_speed(self.speed)}")


def open_archive_source(paths: Paths, server: Server, version: Version, manifest: dict, archive_url: str,
                        entries: list[dict], session: requests.Session, cancel: threading.Event,
                        progress: ProgressFn) -> ZipSource:
    """Partial (HTTP Range) source when worth it, otherwise a full one-time ZIP download."""
    try:
        direct = resolve(archive_url, session)
        remote = RemoteZip(direct, session=session, cancel=cancel)
        try:
            source = ZipSource(remote.zip, remote.file.close, "range")
            infos = [source.info_for(e) for e in entries]
            planned = remote.planned_bytes(infos)
            if planned > remote.size * FULL_DOWNLOAD_RATIO:
                raise RangeNotSupported("Most of the pack changed: a full download is faster")
            counter = _ByteProgress(planned, progress, "Solo lo que cambió: ")
            remote.file.on_bytes = counter
            progress("downloading", 0.0,
                     f"Descargando solo lo que cambió: {_fmt_size(planned)} de {_fmt_size(remote.size)}")
            remote.prefetch(infos)
            return source
        except BaseException:
            remote.close()
            raise
    except (RangeNotSupported, RemoteZipError, ResolveError, requests.RequestException, zipfile.BadZipFile):
        pass

    archive_path = paths.downloads / f"{server.id}-{version.id}-update.zip"
    archive_meta = manifest.get("archive") if isinstance(manifest.get("archive"), dict) else {}
    expected = str(archive_meta.get("sha256") or "")
    if not expected and archive_url == (version.link or "").strip():
        expected = version.sha256 or ""
    progress("downloading", 0.0, "Descargando el paquete completo")

    def on_dl(done: int, total: int | None, speed: float) -> None:
        frac = (done / total) if total else None
        txt = _fmt_size(done) + (f" / {_fmt_size(total)}" if total else "")
        progress("downloading", frac, f"{txt} · {_fmt_speed(speed)}")

    download_file(archive_url, archive_path, progress=on_dl, cancel=cancel, session=session,
                  expected_sha256=expected)
    zf = zipfile.ZipFile(archive_path)
    try:
        return ZipSource(zf, lambda: archive_path.unlink(missing_ok=True), "full")
    except BaseException:
        zf.close()
        archive_path.unlink(missing_ok=True)
        raise


# ------------------------------------------------------------------ install
def _install_from_manifest(paths: Paths, ds: DataSource, server: Server, version: Version,
                           game_dir: Path, prev_files: dict, prev_cache: dict, full_check: bool,
                           cancel: threading.Event, progress: ProgressFn
                           ) -> tuple[installer.ApplyResult, installer.UpdatePlan, str]:
    progress("preparing", None, "Leyendo el manifiesto de archivos")
    manifest = ds.fetch_json(version.manifest)
    if not isinstance(manifest, dict) or not isinstance(manifest.get("files"), list):
        raise ValueError("El manifiesto de esta versión no es válido")

    progress("verifying", None, "Comprobando los archivos instalados")
    plan = installer.plan_update(manifest, game_dir, prev_files, prev_cache, full_check)
    apply_progress = lambda f: progress("applying", f, "")  # noqa: E731
    if not plan.has_changes:
        return installer.apply_plan(plan, game_dir, paths.backups, server.id, None, apply_progress), plan, "none"

    size_mb = plan.download_bytes / (1024 * 1024) if plan.download_bytes else version.size_mb
    _check_disk(game_dir, size_mb, factor=2.0)

    archive_entries = [e for e in plan.downloads if not e.get("url")]
    archive_url = get_manifest_archive_url(manifest, version) if archive_entries else ""
    if archive_entries and not archive_url:
        raise ValueError("La versión necesita un link al ZIP en versions.json para aplicar este manifest")

    source: ZipSource | None = None
    try:
        if archive_entries:
            source = open_archive_source(paths, server, version, manifest, archive_url, archive_entries,
                                         ds.session, cancel, progress)

        def fetch_entry(entry: dict, dest: Path) -> None:
            if entry.get("url"):
                download_file(entry["url"], dest, cancel=cancel, session=ds.session)
            elif source is not None:
                source.extract(entry, dest)
            else:
                raise ValueError(f"No download source configured for {entry['path']}")

        progress("applying", 0.0, "Aplicando solo los archivos que cambiaron")
        applied = installer.apply_plan(plan, game_dir, paths.backups, server.id, fetch_entry, apply_progress)
        return applied, plan, source.mode if source else "files"
    finally:
        if source is not None:
            source.close()


def install_version(paths: Paths, settings: Settings, ds: DataSource, store: installer.StateStore,
                    server: Server, version: Version, mode: str, cancel: threading.Event,
                    progress: ProgressFn, full_check: bool = False,
                    custom_game_dir: Path | str | None = None) -> dict:
    if custom_game_dir:
        game_dir = Path(custom_game_dir).resolve()
    else:
        prev_st = store.get(server.id)
        if prev_st and prev_st.get("game_dir") and mode in {"instance", "default"}:
            game_dir = Path(prev_st["game_dir"]).resolve()
        else:
            game_dir = settings.game_dir(server.id, mode)

    prev = store.get(server.id)
    prev_files = dict(prev["files"]) if prev else {}
    prev_cache = dict(prev.get("file_cache") or {}) if prev else {}

    # Mode changed (instance <-> default folder): move = remove old managed files first.
    if prev and Path(prev["game_dir"]).resolve() != game_dir.resolve():
        installer.remove_managed(Path(prev["game_dir"]), prev_files)
        prev_files, prev_cache = {}, {}

    if mode == "default":                       # only one pack at a time in .minecraft
        installer.release_game_dir(store, game_dir, except_server=server.id)

    label = server.id
    if version.manifest:
        applied, plan, source_mode = _install_from_manifest(
            paths, ds, server, version, game_dir, prev_files, prev_cache, full_check, cancel, progress)
    else:
        if not version.link:
            raise ValueError("This version has no download link")
        _check_disk(game_dir, version.size_mb)
        archive = paths.downloads / f"{server.id}-{version.id}.pkg"
        extracted = paths.downloads / f"{server.id}-{version.id}-extracted"
        try:
            progress("downloading", 0.0, "Conectando con el servidor de descarga")

            def on_dl(done: int, total: int | None, speed: float) -> None:
                frac = (done / total) if total else None
                txt = f"{done / 1048576:.0f}"
                if total:
                    txt += f" / {total / 1048576:.0f}"
                progress("downloading", frac, f"{txt} MB · {_fmt_speed(speed)}")

            download_file(version.link, archive, progress=on_dl, cancel=cancel,
                          session=ds.session, expected_sha256=version.sha256)
            progress("extracting", None, "Descarga completa; comprobando y descomprimiendo el paquete")
            root = extract(archive, extracted)
            progress("applying", 0.0, "Copiando los archivos del modpack a la instancia")
            captured: list = []
            installer.apply_package(
                root, game_dir, prev_files, paths.backups, label,
                progress=lambda f: progress("applying", f, ""), prev_cache=prev_cache,
                full_check=full_check, result=captured)
            applied, plan = captured
            source_mode = "package"
        finally:
            archive.unlink(missing_ok=True)      # the .part file (if any) is kept for resume
            shutil.rmtree(extracted, ignore_errors=True)

    state = installer.new_state(server.id, version.id, mode, game_dir, applied.files, version.version,
                                file_cache=applied.file_cache)
    state["last_update"] = {**plan.summary(), "source": source_mode,
                            "backup": str(applied.backup) if applied.backup else ""}
    store.save(server.id, state)
    progress("done", 1.0, "")
    return state
