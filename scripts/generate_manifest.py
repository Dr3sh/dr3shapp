#!/usr/bin/env python3
"""Generate a Dr3sh Launcher update manifest from ZIP or RAR archives.

Manifest format 2 (what the launcher consumes):

    {
      "format": 2,
      "version": "1.2.0",
      "complete": true,                      # files = the FULL index of the pack
      "archive": {"name": "...zip", "size": 123, "sha256": "..."},
      "files": [{"path": "mods/a.jar", "sha256": "...", "size": 1, "archive_path": "Pack/mods/a.jar"}],
      "remove": ["mods/old.jar"],            # informative: complete manifests already imply it
      "changes": {"from": "1.1.0", "added": [...], "updated": [...], "removed": [...]}
    }

The index is always complete so a player on ANY older version (or with a broken
install) can reach this version: the launcher compares every file against its local
state and downloads only what differs, straight from the release ZIP (HTTP Range).
``--old`` / ``--old-manifest`` are only used to describe the changes (changelog/UI).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import stat
import subprocess
import sys
import tempfile
import time
import zipfile
from pathlib import Path, PurePosixPath

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from core.archive import detect_wrapper_prefix, is_junk  # noqa: E402

FORMAT = 2


def _normalize_member(name: str, strip_prefix: str | None) -> str:
    normalized = name.replace("\\", "/")
    path = PurePosixPath(normalized)
    if path.is_absolute() or not path.parts or ".." in path.parts or ":" in path.parts[0]:
        raise ValueError(f"Unsafe ZIP path: {name}")

    result = path.as_posix()
    if strip_prefix:
        prefix = strip_prefix.replace("\\", "/").strip("/")
        if result == prefix:
            raise ValueError(f"ZIP entry is the prefix directory, not a file: {name}")
        if not result.startswith(prefix + "/"):
            raise ValueError(f"ZIP path is outside --strip-prefix '{prefix}': {name}")
        result = result[len(prefix) + 1:]
    return result


def _hash_stream(file) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    for chunk in iter(lambda: file.read(1024 * 1024), b""):
        digest.update(chunk)
        size += len(chunk)
    return digest.hexdigest(), size


def _zip_hashes(archive_path: Path, strip_prefix: str | None,
                auto_strip: bool = True) -> dict[str, tuple[str, str, int]]:
    hashes = {}
    with zipfile.ZipFile(archive_path) as archive:
        infos = []
        for info in archive.infolist():
            if info.is_dir():
                continue
            mode = info.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise ValueError(f"Symbolic links are not supported in ZIPs: {info.filename}")
            member_path = PurePosixPath(info.filename.replace("\\", "/")).as_posix()
            _normalize_member(member_path, None)
            if is_junk(member_path):
                continue
            infos.append((info, member_path))
        if strip_prefix is None and auto_strip:
            strip_prefix = detect_wrapper_prefix([m for _, m in infos])
        for info, member_path in infos:
            relative_path = _normalize_member(member_path, strip_prefix)
            if relative_path in hashes:
                raise ValueError(f"Duplicate ZIP path after normalization: {relative_path}")
            with archive.open(info) as file:
                digest, size = _hash_stream(file)
            hashes[relative_path] = (digest, member_path, size)
    return hashes


def _rar_hashes(archive_path: Path, strip_prefix: str | None,
                auto_strip: bool = True) -> dict[str, tuple[str, str, int]]:
    from core.archive import find_7z

    executable = find_7z()
    if not executable:
        raise ValueError("RAR support needs 7-Zip. Install it or add 7z.exe to assets/bin/")

    listing = subprocess.run(
        [executable, "l", "-slt", "-sccUTF-8", str(archive_path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if listing.returncode not in (0, 1):
        raise ValueError(f"7-Zip could not read {archive_path}: {(listing.stderr or listing.stdout).strip()[-300:]}")

    records = []
    current = {}
    reading_entries = False
    for line in listing.stdout.splitlines():
        if line.strip() == "----------":
            if reading_entries and current:
                records.append(current)
                current = {}
            reading_entries = True
            continue
        if not reading_entries:
            continue
        if not line.strip():
            if current:
                records.append(current)
                current = {}
            continue
        key, separator, value = line.partition(" = ")
        if separator:
            current[key] = value
    if current:
        records.append(current)

    members = []
    seen = set()
    for record in records:
        if not record.get("Path") or record.get("Folder") == "+":
            continue
        member_path = PurePosixPath(record["Path"].replace("\\", "/")).as_posix()
        _normalize_member(member_path, None)
        if record.get("Symbolic Link") or record.get("Hard Link"):
            raise ValueError(f"Links are not supported in RAR archives: {member_path}")
        if member_path in seen:
            raise ValueError(f"Duplicate RAR path: {member_path}")
        seen.add(member_path)
        if not is_junk(member_path):
            members.append(member_path)
    if strip_prefix is None and auto_strip:
        strip_prefix = detect_wrapper_prefix(members)

    hashes = {}
    with tempfile.TemporaryDirectory(prefix="dr3sh-manifest-rar-") as temp_dir:
        extract_dir = Path(temp_dir)
        extracted = subprocess.run(
            [executable, "x", "-y", "-sccUTF-8", f"-o{extract_dir}", str(archive_path)],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        if extracted.returncode not in (0, 1):
            raise ValueError(f"7-Zip could not extract {archive_path}: {(extracted.stderr or extracted.stdout).strip()[-300:]}")

        for member_path in members:
            source = (extract_dir / Path(*PurePosixPath(member_path).parts)).resolve()
            if extract_dir.resolve() not in source.parents or not source.is_file() or source.is_symlink():
                raise ValueError(f"RAR member was not extracted as a regular file: {member_path}")
            relative_path = _normalize_member(member_path, strip_prefix)
            if relative_path in hashes:
                raise ValueError(f"Duplicate RAR path after normalization: {relative_path}")
            with source.open("rb") as file:
                digest, size = _hash_stream(file)
            hashes[relative_path] = (digest, member_path, size)
    return hashes


def _archive_hashes(archive_path: Path, strip_prefix: str | None) -> dict[str, tuple[str, str, int]]:
    suffix = archive_path.suffix.lower()
    if suffix == ".zip":
        return _zip_hashes(archive_path, strip_prefix)
    if suffix == ".rar":
        return _rar_hashes(archive_path, strip_prefix)
    raise ValueError(f"Unsupported archive type: {archive_path.name} (use .zip or .rar)")


def _file_sha256(path: Path) -> str:
    with path.open("rb") as file:
        return _hash_stream(file)[0]


def _index_from_manifest(manifest: dict) -> dict[str, str] | None:
    """{path: sha256} of a previous manifest, or None when it is not a complete index."""
    if not isinstance(manifest, dict) or manifest.get("complete") is not True:
        return None
    index = {}
    for entry in manifest.get("files") or []:
        if isinstance(entry, dict) and entry.get("path"):
            index[str(entry["path"]).replace("\\", "/")] = str(entry.get("sha256") or "").lower()
    return index


def build_manifest(
    new_archive: Path,
    version: str,
    old_archive: Path | None = None,
    strip_prefix: str | None = None,
    old_manifest: dict | None = None,
) -> dict:
    if not version.strip():
        raise ValueError("Version cannot be empty")

    new_files = _archive_hashes(new_archive, strip_prefix)
    if not new_files:
        raise ValueError("The archive does not contain any files")

    old_index: dict[str, str] | None = None
    from_version = ""
    if old_archive:
        old_index = {path: digest for path, (digest, _, _) in _archive_hashes(old_archive, strip_prefix).items()}
    elif old_manifest is not None:
        old_index = _index_from_manifest(old_manifest)
        from_version = str(old_manifest.get("version") or "") if isinstance(old_manifest, dict) else ""

    files = []
    for path, (digest, archive_path, size) in sorted(new_files.items()):
        if old_archive is not None and old_index is not None and old_index.get(path) == digest:
            continue
        entry = {"path": path, "sha256": digest, "size": size}
        if archive_path != path:
            entry["archive_path"] = archive_path
        files.append(entry)

    manifest: dict = {
        "format": FORMAT,
        "version": version.strip(),
        "complete": old_archive is None,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "files": files,
        "remove": [],
    }
    if new_archive.suffix.lower() == ".zip":
        manifest["archive"] = {
            "name": new_archive.name,
            "size": new_archive.stat().st_size,
            "sha256": _file_sha256(new_archive),
        }
    if old_index is not None:
        added = sorted(p for p in new_files if p not in old_index)
        updated = sorted(p for p, (digest, _, _) in new_files.items() if p in old_index and old_index[p] != digest)
        removed = sorted(p for p in (set(old_index) - set(new_files)) if not p.startswith("config/"))
        manifest["remove"] = removed
        manifest["changes"] = {"from": from_version, "added": added, "updated": updated, "removed": removed}
    return manifest


def describe_changes(manifest: dict) -> str:
    """Short human summary, e.g. '+2 añadidos · 3 actualizados · -1 eliminados'."""
    changes = manifest.get("changes") or {}
    if not changes:
        return ""
    parts = []
    if changes.get("added"):
        parts.append(f"+{len(changes['added'])} añadidos")
    if changes.get("updated"):
        parts.append(f"{len(changes['updated'])} actualizados")
    if changes.get("removed"):
        parts.append(f"-{len(changes['removed'])} eliminados")
    return " · ".join(parts) or "Sin cambios de archivos"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Index a ZIP or RAR modpack and generate a launcher update manifest."
    )
    parser.add_argument("--new", required=True, type=Path, help="ZIP or RAR for the new modpack version")
    parser.add_argument("--old", type=Path, help="ZIP or RAR of the previous version (only for the change summary)")
    parser.add_argument("--old-manifest", type=Path,
                        help="manifest.json of the previous version (only for the change summary)")
    parser.add_argument("--version", required=True, help="Release version, for example 1.2.0")
    parser.add_argument("--output", type=Path, default=Path("manifest.json"), help="Output JSON path")
    parser.add_argument(
        "--strip-prefix",
        help="Explicit top-level directory to remove from paths (auto-detected when omitted)",
    )
    args = parser.parse_args()

    try:
        old_manifest = None
        if args.old_manifest:
            old_manifest = json.loads(args.old_manifest.read_text(encoding="utf-8-sig"))
        manifest = build_manifest(args.new, args.version, args.old, args.strip_prefix, old_manifest)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    except (OSError, ValueError, zipfile.BadZipFile, zipfile.LargeZipFile) as error:
        parser.error(str(error))

    print(f"Manifest saved: {args.output}")
    print(f"Files indexed: {len(manifest['files'])}")
    summary = describe_changes(manifest)
    if summary:
        print(f"Changes: {summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())