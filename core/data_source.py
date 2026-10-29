"""Sync with the versions.json hosted on GitHub (ETag + offline cache + image cache)."""
from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path
from urllib.parse import urljoin, urlparse

import requests

from .config import Paths, Settings, read_json, write_json
from .models import Catalog, CatalogError, parse_catalog

TIMEOUT = 15

_GITHUB_RAW_RE = re.compile(
    r"^https?://raw\.githubusercontent\.com/([^/]+)/([^/]+)/(?:refs/heads/)?([^/]+)/(.+)$"
)


class DataSource:
    def __init__(self, paths: Paths, settings: Settings):
        self.paths = paths
        self.settings = settings
        self.session = requests.Session()
        self.session.headers["User-Agent"] = "Dr3shLauncher"

    # ---- catalog -----------------------------------------------------
    def load_cached(self) -> dict:
        """Last valid synced JSON, or the bundled sample on first run."""
        for p in (self.paths.catalog_cache, self.paths.assets / "versions.sample.json"):
            data = read_json(p, None)
            if data is not None:
                try:
                    parse_catalog(data)
                    self._previous_snapshot = data
                    return data
                except CatalogError:
                    continue
        return {"servers": [], "versions": []}

    def _resolve_live_github_raw(self, url: str) -> tuple[str, str | None]:
        """Resolve branch ref on raw.githubusercontent.com to its exact real-time commit SHA.

        GitHub's Fastly CDN caches branch URLs (e.g. refs/heads/main) for 300 seconds (5 minutes)
        and ignores query strings. Querying the Git smart-HTTP refs endpoint returns the live
        commit SHA with max-age=0 and no API rate limit, and fetching raw.githubusercontent.com
        by commit SHA is a guaranteed instant cache miss on new commits.
        """
        m = _GITHUB_RAW_RE.match(url.split("?")[0])
        if not m:
            return url, None
        owner, repo, branch, file_path = m.groups()
        if re.fullmatch(r"[0-9a-f]{40}", branch):
            return url, branch
        try:
            refs_url = f"https://github.com/{owner}/{repo}.git/info/refs?service=git-upload-pack"
            r = self.session.get(
                refs_url,
                headers={"Cache-Control": "no-cache", "Pragma": "no-cache"},
                timeout=6,
            )
            if r.ok:
                sha_match = re.search(rf"([0-9a-f]{{40}})\s+refs/heads/{re.escape(branch)}\b", r.text)
                if sha_match:
                    sha = sha_match.group(1)
                    return f"https://raw.githubusercontent.com/{owner}/{repo}/{sha}/{file_path}", sha
        except requests.RequestException:
            pass
        return url, None

    def sync(self, force: bool = False) -> tuple[bool, dict | None]:
        """Returns (changed, data). Raises on network/validation errors."""
        headers = {"Cache-Control": "no-cache", "Pragma": "no-cache"}
        url = self.settings["json_url"]
        live_url, commit_sha = self._resolve_live_github_raw(url)

        if commit_sha:
            if not force and self.paths.etag.exists() and self.paths.catalog_cache.exists():
                cached_tag = self.paths.etag.read_text(encoding="utf-8").strip()
                if cached_tag == commit_sha:
                    return False, None
            r = self.session.get(live_url, headers=headers, timeout=TIMEOUT)
        else:
            if not force and self.paths.etag.exists() and self.paths.catalog_cache.exists():
                headers["If-None-Match"] = self.paths.etag.read_text(encoding="utf-8").strip()
            r = self.session.get(url, headers=headers, timeout=TIMEOUT)
            if r.status_code == 304:
                return False, None

        r.raise_for_status()
        data = json.loads(r.content.decode("utf-8-sig"))
        parse_catalog(data)                      # validate before replacing the cache
        write_json(self.paths.catalog_cache, data)
        tag_to_save = commit_sha or r.headers.get("ETag")
        if tag_to_save:
            self.paths.etag.write_text(tag_to_save, encoding="utf-8")
        old = self._previous_snapshot
        self._previous_snapshot = data
        return old != data, data

    _previous_snapshot: dict | None = None

    # ---- urls / images -----------------------------------------------
    def resolve_url(self, rel: str) -> str:
        """Relative paths from the JSON are resolved against the GitHub raw base URL."""
        if urlparse(rel).scheme in ("http", "https"):
            return rel
        return urljoin(self.settings["assets_base_url"], rel)

    def image_path(self, rel: str) -> Path | None:
        """Local cached path for a JSON image (downloaded on demand), or None."""
        if not rel:
            return None
        url = self.resolve_url(rel)
        ext = Path(urlparse(url).path).suffix or ".img"
        dest = self.paths.images / (hashlib.sha1(url.encode()).hexdigest() + ext)
        if dest.exists() and dest.stat().st_size > 0:
            return dest
        try:
            r = self.session.get(url, timeout=TIMEOUT)
            r.raise_for_status()
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(r.content)
            return dest
        except requests.RequestException:
            return None

    def fetch_json(self, rel_or_url: str) -> dict:
        r = self.session.get(self.resolve_url(rel_or_url), timeout=TIMEOUT)
        r.raise_for_status()
        return json.loads(r.content.decode("utf-8-sig"))


def catalog_from(data: dict) -> Catalog:
    return parse_catalog(data)
