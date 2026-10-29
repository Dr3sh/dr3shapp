"""Streaming downloads with progress, speed, resume (HTTP Range), retries and cancel."""
from __future__ import annotations

import hashlib
import os
import threading
import time
from pathlib import Path
from typing import Callable

import requests

from .resolvers import ResolveError, resolve

ProgressCb = Callable[[int, int | None, float], None]   # done, total, bytes/sec
CHUNK = 1 << 16


class Cancelled(Exception):
    pass


class DownloadError(Exception):
    pass


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _stream(url: str, part: Path, session: requests.Session, progress: ProgressCb | None,
            cancel: threading.Event | None) -> None:
    existing = part.stat().st_size if part.exists() else 0
    headers = {"Range": f"bytes={existing}-"} if existing else {}
    with session.get(url, headers=headers, stream=True, timeout=30, allow_redirects=True) as r:
        if r.status_code == 416:                       # already fully downloaded
            return
        r.raise_for_status()
        if "text/html" in r.headers.get("Content-Type", "").lower():
            raise DownloadError("The link returned a web page instead of a file")
        resumed = r.status_code == 206
        if existing and not resumed:                   # server ignored Range -> restart
            existing = 0
        length = r.headers.get("Content-Length")
        total = (int(length) + existing) if length and length.isdigit() else None
        done = existing
        mode = "ab" if resumed and existing else "wb"
        last_t, last_done, speed = time.monotonic(), done, 0.0
        with open(part, mode) as f:
            for chunk in r.iter_content(CHUNK):
                if cancel is not None and cancel.is_set():
                    raise Cancelled()
                if not chunk:
                    continue
                f.write(chunk)
                done += len(chunk)
                now = time.monotonic()
                if now - last_t >= 0.25:
                    speed = (done - last_done) / (now - last_t)
                    last_t, last_done = now, done
                    if progress:
                        progress(done, total, speed)
        if progress:
            progress(done, total, speed)


def download_file(url: str, dest: Path, progress: ProgressCb | None = None,
                  cancel: threading.Event | None = None, session: requests.Session | None = None,
                  retries: int = 3, expected_sha256: str = "") -> Path:
    session = session or requests.Session()
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_name(dest.name + ".part")
    last_err: Exception | None = None
    for attempt in range(retries):
        try:
            direct = resolve(url, session)             # re-resolve each try (links can expire)
            _stream(direct, part, session, progress, cancel)
            last_err = None
            break
        except Cancelled:
            raise
        except ResolveError as e:
            raise DownloadError(str(e)) from e
        except (requests.RequestException, DownloadError) as e:
            last_err = e
            if cancel is not None and cancel.wait(1.5 * (attempt + 1)):
                raise Cancelled()
    if last_err is not None:
        raise DownloadError(f"Download failed after {retries} attempts: {last_err}")
    os.replace(part, dest)
    if expected_sha256:
        got = sha256_file(dest)
        if got.lower() != expected_sha256.lower():
            dest.unlink(missing_ok=True)
            raise DownloadError("Checksum mismatch (sha256): the file is corrupted")
    return dest
