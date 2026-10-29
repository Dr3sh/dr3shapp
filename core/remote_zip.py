"""Read individual files out of a remote ZIP using HTTP Range requests.

This is what makes updates "Steam-like": a release only publishes ONE full ZIP, and a
client that already has most of the pack downloads just the bytes of the files that
changed (plus the ZIP central directory, a few KB). It works for any version jump
(v1 -> v5) and for repairs, without patch chains.

GitHub release assets support Range requests (they redirect to a signed CDN URL).
If a server does not support ranges, :class:`RangeNotSupported` is raised and the
caller falls back to a normal full download.
"""
from __future__ import annotations

import io
import re
import tempfile
import threading
import time
import zipfile
from typing import Callable, Iterable

import requests

BLOCK = 64 * 1024                 # cache granularity
MERGE_GAP = 512 * 1024            # fetch small holes between needed ranges instead of a new request
LOCAL_HEADER_SLACK = 1024         # local headers may carry a bigger "extra" field than the central dir


class RangeNotSupported(Exception):
    pass


class RemoteZipError(Exception):
    pass


class Cancelled(Exception):
    pass


BytesCb = Callable[[int], None]   # called with the number of new bytes received


class HttpRangeFile(io.RawIOBase):
    """Seekable, read-only file object backed by HTTP Range requests and a disk block cache."""

    def __init__(self, url: str, session: requests.Session | None = None,
                 cancel: threading.Event | None = None, on_bytes: BytesCb | None = None,
                 timeout: int = 30, retries: int = 3):
        super().__init__()
        self.url = url
        self.session = session or requests.Session()
        self.cancel = cancel
        self.on_bytes = on_bytes
        self.timeout = timeout
        self.retries = retries
        self._resolved = url
        self._pos = 0
        self._blocks: dict[int, int] = {}            # block index -> offset in the spool file
        self._spool = tempfile.TemporaryFile(prefix="dr3sh-range-")
        self._spool_end = 0
        self.requests_made = 0
        self.bytes_fetched = 0
        self.size = self._probe()

    # ------------------------------------------------------------ http
    def _probe(self) -> int:
        with self.session.get(self.url, headers={"Range": "bytes=0-0"}, stream=True,
                              timeout=self.timeout, allow_redirects=True) as r:
            if r.status_code != 206:
                raise RangeNotSupported(f"Server answered {r.status_code} to a Range request")
            if "text/html" in r.headers.get("Content-Type", "").lower():
                raise RangeNotSupported("The link returned a web page instead of a file")
            match = re.match(r"bytes\s+\d+-\d+/(\d+)", r.headers.get("Content-Range", ""))
            if not match:
                raise RangeNotSupported("Missing Content-Range total size")
            self._resolved = r.url or self.url      # signed CDN URL (saves a redirect per request)
            return int(match.group(1))

    def _fetch_run(self, first: int, last: int) -> None:
        """Download blocks [first, last] in a single request and store the missing ones."""
        start = first * BLOCK
        end = min((last + 1) * BLOCK, self.size) - 1
        last_error: Exception | None = None
        for attempt in range(self.retries):
            if self.cancel is not None and self.cancel.is_set():
                raise Cancelled()
            url = self._resolved if attempt == 0 else self.url   # signed URLs expire -> re-resolve
            try:
                with self.session.get(url, headers={"Range": f"bytes={start}-{end}"}, stream=True,
                                      timeout=self.timeout, allow_redirects=True) as r:
                    if r.status_code in (401, 403, 404, 410) and url != self.url:
                        last_error = RemoteZipError(f"Signed URL rejected ({r.status_code})")
                        continue
                    if r.status_code != 206:
                        raise RangeNotSupported(f"Server answered {r.status_code} to a Range request")
                    got = re.match(r"bytes\s+(\d+)-", r.headers.get("Content-Range", ""))
                    if not got or int(got.group(1)) != start:
                        raise RemoteZipError("Server returned an unexpected byte range")
                    if url == self.url:
                        self._resolved = r.url or self.url
                    self.requests_made += 1
                    buffer = bytearray()
                    block = first
                    for chunk in r.iter_content(BLOCK):
                        if self.cancel is not None and self.cancel.is_set():
                            raise Cancelled()
                        if not chunk:
                            continue
                        buffer += chunk
                        self.bytes_fetched += len(chunk)
                        if self.on_bytes:
                            self.on_bytes(len(chunk))
                        while len(buffer) >= BLOCK:
                            self._store(block, bytes(buffer[:BLOCK]))
                            del buffer[:BLOCK]
                            block += 1
                    if buffer:
                        expected = min((block + 1) * BLOCK, self.size) - block * BLOCK
                        if len(buffer) != expected:
                            raise RemoteZipError("Connection closed before the range was complete")
                        self._store(block, bytes(buffer))
                        block += 1
                    if block <= last and block * BLOCK < self.size:
                        raise RemoteZipError("Connection closed before the range was complete")
                    return
            except (requests.RequestException, RemoteZipError) as exc:
                last_error = exc
                if self.cancel is not None and self.cancel.wait(1.0 * (attempt + 1)):
                    raise Cancelled()
                if self.cancel is None:
                    time.sleep(0.5 * (attempt + 1))
        raise RemoteZipError(f"Range download failed after {self.retries} attempts: {last_error}")

    def _store(self, block: int, data: bytes) -> None:
        if block in self._blocks:
            return
        self._spool.seek(self._spool_end)
        self._spool.write(data)
        self._blocks[block] = self._spool_end
        self._spool_end += len(data)

    # ------------------------------------------------------------ planning
    def _missing_runs(self, blocks: Iterable[int], merge_gap_blocks: int) -> list[tuple[int, int]]:
        missing = sorted(b for b in set(blocks) if b not in self._blocks)
        runs: list[list[int]] = []
        for b in missing:
            if runs and b - runs[-1][1] <= merge_gap_blocks + 1:
                runs[-1][1] = b
            else:
                runs.append([b, b])
        return [(a, z) for a, z in runs]

    def ensure(self, start: int, end: int) -> None:
        """Make sure bytes [start, end) are cached."""
        if end <= start:
            return
        first, last = start // BLOCK, (min(end, self.size) - 1) // BLOCK
        for a, z in self._missing_runs(range(first, last + 1), 0):
            self._fetch_run(a, z)

    def prefetch(self, ranges: Iterable[tuple[int, int]], merge_gap: int = MERGE_GAP) -> None:
        """Fetch many [start, end) ranges with as few requests as possible."""
        blocks: set[int] = set()
        for start, end in ranges:
            end = min(end, self.size)
            if end > start:
                blocks.update(range(start // BLOCK, (end - 1) // BLOCK + 1))
        for a, z in self._missing_runs(blocks, merge_gap // BLOCK):
            self._fetch_run(a, z)

    def planned_bytes(self, ranges: Iterable[tuple[int, int]], merge_gap: int = MERGE_GAP) -> int:
        blocks: set[int] = set()
        for start, end in ranges:
            end = min(end, self.size)
            if end > start:
                blocks.update(range(start // BLOCK, (end - 1) // BLOCK + 1))
        total = 0
        for a, z in self._missing_runs(blocks, merge_gap // BLOCK):
            total += min((z + 1) * BLOCK, self.size) - a * BLOCK
        return total

    # ------------------------------------------------------------ io api
    def readable(self) -> bool:
        return True

    def seekable(self) -> bool:
        return True

    def tell(self) -> int:
        return self._pos

    def seek(self, offset: int, whence: int = io.SEEK_SET) -> int:
        if whence == io.SEEK_SET:
            pos = offset
        elif whence == io.SEEK_CUR:
            pos = self._pos + offset
        elif whence == io.SEEK_END:
            pos = self.size + offset
        else:
            raise ValueError("invalid whence")
        if pos < 0:
            raise ValueError("negative seek position")
        self._pos = pos
        return pos

    def readinto(self, buffer) -> int:
        view = memoryview(buffer).cast("B")
        want = min(len(view), max(self.size - self._pos, 0))
        if want <= 0:
            return 0
        self.ensure(self._pos, self._pos + want)
        done = 0
        while done < want:
            pos = self._pos + done
            block, inner = divmod(pos, BLOCK)
            take = min(BLOCK - inner, want - done)
            self._spool.seek(self._blocks[block] + inner)
            data = self._spool.read(take)
            if not data:
                break
            view[done:done + len(data)] = data
            done += len(data)
        self._pos += done
        return done

    def close(self) -> None:
        try:
            self._spool.close()
        finally:
            super().close()


def member_range(info: zipfile.ZipInfo) -> tuple[int, int]:
    """Byte range that contains the local header + compressed data of a ZIP member."""
    name_len = len(info.orig_filename.encode("utf-8"))
    start = info.header_offset
    end = start + 30 + name_len + len(info.extra or b"") + LOCAL_HEADER_SLACK + info.compress_size
    return start, end


class RemoteZip:
    """Open a remote ZIP and extract only selected members."""

    def __init__(self, url: str, session: requests.Session | None = None,
                 cancel: threading.Event | None = None, on_bytes: BytesCb | None = None):
        self.file = HttpRangeFile(url, session=session, cancel=cancel, on_bytes=on_bytes)
        # The central directory lives at the end: grab the tail in one request.
        tail = min(self.file.size, 256 * 1024)
        self.file.ensure(self.file.size - tail, self.file.size)
        try:
            self.zip = zipfile.ZipFile(self.file)
        except zipfile.BadZipFile as exc:
            self.file.close()
            raise RemoteZipError(f"The remote file is not a valid ZIP: {exc}") from exc

    @property
    def size(self) -> int:
        return self.file.size

    def planned_bytes(self, infos: Iterable[zipfile.ZipInfo]) -> int:
        return self.file.planned_bytes(member_range(i) for i in infos)

    def prefetch(self, infos: Iterable[zipfile.ZipInfo]) -> None:
        self.file.prefetch(member_range(i) for i in infos)

    def close(self) -> None:
        try:
            self.zip.close()
        finally:
            self.file.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
