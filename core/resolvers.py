"""Turn the `link` values of versions.json into real direct-download URLs.

One function per host so new hosts are easy to add.
"""
from __future__ import annotations

import html
import re
from urllib.parse import parse_qs, urlencode, urlparse

import requests

TIMEOUT = 20


class ResolveError(Exception):
    pass


def _host(url: str) -> str:
    return (urlparse(url).hostname or "").lower()


def resolve_mediafire(url: str, session: requests.Session) -> str:
    host = _host(url)
    if host.startswith("download") and host.endswith("mediafire.com"):
        return url                                   # already a direct (possibly expiring) link
    r = session.get(url, timeout=TIMEOUT)
    r.raise_for_status()
    page = r.text
    patterns = (
        r'aria-label="Download file"[^>]*href="([^"]+)"',
        r'href="([^"]+)"[^>]*aria-label="Download file"',
        r'(https?://download[0-9]*\.mediafire\.com/[^"\'\s<>]+)',
    )
    for pat in patterns:
        m = re.search(pat, page)
        if m:
            direct = html.unescape(m.group(1))
            if direct.startswith("//"):
                direct = "https:" + direct
            return direct
    raise ResolveError("Could not find the direct download link on the MediaFire page")


def _gdrive_id(url: str) -> str | None:
    qs = parse_qs(urlparse(url).query)
    if qs.get("id"):
        return qs["id"][0]
    m = re.search(r"/d/([A-Za-z0-9_-]+)", url)
    return m.group(1) if m else None


def resolve_gdrive(url: str, session: requests.Session) -> str:
    file_id = _gdrive_id(url)
    if not file_id:
        raise ResolveError("Invalid Google Drive link (no file id)")
    first = f"https://drive.google.com/uc?export=download&id={file_id}"
    r = session.get(first, stream=True, timeout=TIMEOUT)
    ctype = r.headers.get("Content-Type", "")
    if "text/html" not in ctype:
        r.close()
        return first                                 # small file: direct download
    page = r.text                                    # large file: virus-scan confirmation page
    r.close()
    form = re.search(r'<form[^>]+id="download-form"[^>]+action="([^"]+)"', page) \
        or re.search(r'<form[^>]+action="([^"]+)"[^>]+id="download-form"', page)
    if form:
        params = dict(re.findall(r'name="([^"]+)"\s+value="([^"]*)"', page))
        return html.unescape(form.group(1)) + "?" + urlencode(params)
    m = re.search(r'confirm=([0-9A-Za-z_-]+)', page)
    if m:
        return first + "&confirm=" + m.group(1)
    raise ResolveError("Google Drive did not return a downloadable file (quota or permissions?)")


def resolve(url: str, session: requests.Session | None = None) -> str:
    """Return a direct URL for `url`. Raises ResolveError for non-downloadable links."""
    session = session or requests.Session()
    host = _host(url)
    if not url.lower().startswith(("http://", "https://")):
        raise ResolveError("Unsupported link (must be http/https)")
    if host.endswith("youtube.com") or host.endswith("youtu.be"):
        raise ResolveError("This link is a video page, not a downloadable file")
    if host.endswith("mediafire.com"):
        return resolve_mediafire(url, session)
    if host.endswith("drive.google.com") or host.endswith("drive.usercontent.google.com"):
        return resolve_gdrive(url, session)
    return url                                       # GitHub releases and any direct link
