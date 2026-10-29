"""Microsoft account validation through minecraft-launcher-lib."""
from __future__ import annotations

import requests

from .minecraft import LaunchError, _import_mll


def _authenticate_xbl_with_fallback(account_api, microsoft_access_token: str) -> dict:
    xbox: dict = {}
    try:
        res = account_api.authenticate_with_xbl(microsoft_access_token)
        if isinstance(res, dict):
            xbox = res
    except Exception:
        xbox = {}

    user_claims = xbox.get("DisplayClaims", {}).get("xui", [])
    if xbox.get("Token") and user_claims and user_claims[0].get("uhs"):
        return xbox
    # Official Minecraft Launcher OAuth (00000000402b5328 / MBI_SSL) uses raw RpsTicket or t= prefix
    raw_token = str(microsoft_access_token or "").strip()
    candidates = [raw_token]
    if not raw_token.startswith("t="):
        candidates.append(f"t={raw_token}")
    if not raw_token.startswith("d="):
        candidates.append(f"d={raw_token}")

    for rps_ticket in candidates:
        try:
            r = requests.post(
                "https://user.auth.xboxlive.com/user/authenticate",
                json={
                    "Properties": {
                        "AuthMethod": "RPS",
                        "SiteName": "user.auth.xboxlive.com",
                        "RpsTicket": rps_ticket,
                    },
                    "RelyingParty": "http://auth.xboxlive.com",
                    "TokenType": "JWT",
                },
                headers={
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "x-xbl-contract-version": "1",
                    "User-Agent": "Dr3shLauncher",
                },
                timeout=15,
            )
            if not r.text:
                continue
            data = r.json()
            claims = data.get("DisplayClaims", {}).get("xui", [])
            if data.get("Token") and claims and claims[0].get("uhs"):
                return data
        except Exception:
            continue
    return xbox


def _authenticate_xsts_with_fallback(account_api, xbl_token: str) -> dict:
    xsts: dict = {}
    try:
        res = account_api.authenticate_with_xsts(xbl_token)
        if isinstance(res, dict):
            xsts = res
    except Exception:
        xsts = {}

    if xsts.get("Token"):
        return xsts

    if not xsts:
        try:
            r = requests.post(
                "https://xsts.auth.xboxlive.com/xsts/authorize",
                json={
                    "Properties": {
                        "SandboxId": "RETAIL",
                        "UserTokens": [xbl_token],
                    },
                    "RelyingParty": "rp://api.minecraftservices.com/",
                    "TokenType": "JWT",
                },
                headers={
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "x-xbl-contract-version": "1",
                    "User-Agent": "Dr3shLauncher",
                },
                timeout=15,
            )
            if r.text:
                xsts = r.json()
        except Exception:
            xsts = {}

    xerr = xsts.get("XErr")
    if xerr == 2148916233:
        raise LaunchError("Esta cuenta Microsoft no tiene perfil de Xbox Live. Inicia sesión una vez en xbox.com para crear tu Gamertag.")
    if xerr == 2148916235:
        raise LaunchError("Xbox Live no está disponible en la región de tu cuenta.")
    if xerr in (2148916236, 2148916237):
        raise LaunchError("Tu cuenta Microsoft requiere verificación de edad en Xbox Live.")
    if xerr == 2148916238:
        raise LaunchError("Esta cuenta es de menor de edad y debe añadirse a un grupo familiar de Microsoft para jugar.")
    return xsts


def _authenticate_minecraft_with_fallback(account_api, userhash: str, xsts_token: str) -> dict:
    minecraft: dict = {}
    try:
        res = account_api.authenticate_with_minecraft(userhash, xsts_token)
        if isinstance(res, dict):
            minecraft = res
    except Exception:
        minecraft = {}

    if minecraft.get("access_token"):
        return minecraft

    xtoken = f"XBL3.0 x={userhash};{xsts_token}"
    endpoints = (
        ("https://api.minecraftservices.com/authentication/login_with_xbox", {"identityToken": xtoken}),
        ("https://api.minecraftservices.com/launcher/login", {"xtoken": xtoken, "platform": "PC_LAUNCHER"}),
    )
    for url, payload in endpoints:
        try:
            r = requests.post(
                url,
                json=payload,
                headers={
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "User-Agent": "Dr3shLauncher",
                },
                timeout=15,
            )
            if not r.text:
                continue
            data = r.json()
            if isinstance(data, dict) and data.get("access_token"):
                return data
        except Exception:
            continue
    return minecraft


def complete_microsoft_login(microsoft_access_token: str, refresh_token: str, include_access_token: bool = False) -> dict:
    if not microsoft_access_token or not refresh_token:
        raise LaunchError("Microsoft no devolvió los tokens necesarios para iniciar sesión.")

    account_api = _import_mll().microsoft_account
    xbox = _authenticate_xbl_with_fallback(account_api, microsoft_access_token)
    user_claims = xbox.get("DisplayClaims", {}).get("xui", [])
    if not xbox.get("Token") or not user_claims or not user_claims[0].get("uhs"):
        raise LaunchError("Microsoft no pudo validar esta cuenta con Xbox Live.")

    xsts = _authenticate_xsts_with_fallback(account_api, xbox["Token"])
    if not xsts.get("Token"):
        raise LaunchError("Xbox Live no autorizó esta cuenta para Minecraft.")

    minecraft = _authenticate_minecraft_with_fallback(account_api, user_claims[0]["uhs"], xsts["Token"])
    access_token = minecraft.get("access_token")
    if not access_token:
        raise LaunchError("Esta aplicación no está autorizada para Minecraft o la cuenta no tiene acceso al juego.")

    profile = account_api.get_profile(access_token)
    if profile.get("error") == "NOT_FOUND":
        raise LaunchError("La cuenta Microsoft no tiene un perfil de Minecraft Java Edition.")
    if not profile.get("id") or not profile.get("name"):
        raise LaunchError("No se encontró un perfil de Minecraft Java Edition para esta cuenta.")

    result = {
        "uuid": profile["id"],
        "username": profile["name"],
        "refresh_token": refresh_token,
    }
    if include_access_token:
        result["access_token"] = access_token
        result["id"] = profile["id"]
        result["name"] = profile["name"]
    return result


def refresh_microsoft_session(client_id: str, refresh_token: str, mll=None) -> dict:
    mll_mod = mll or _import_mll()

    scopes = [
        ("https://login.live.com/oauth20_token.srf", "service::user.auth.xboxlive.com::MBI_SSL"),
        ("https://login.live.com/oauth20_token.srf", None),
        ("https://login.live.com/oauth20_token.srf", "XboxLive.signin offline_access"),
        ("https://login.microsoftonline.com/consumers/oauth2/v2.0/token", "XboxLive.signin offline_access"),
    ]
    if client_id != "00000000402b5328":
        try:
            profile = mll_mod.microsoft_account.complete_refresh(client_id, None, None, refresh_token)
            if isinstance(profile, dict) and profile.get("access_token") and profile.get("id") and profile.get("name"):
                return profile
        except Exception:
            pass

    last_exc: Exception | None = None
    for token_url, scope in scopes:
        try:
            body = {
                "client_id": client_id,
                "refresh_token": refresh_token,
                "grant_type": "refresh_token",
            }
            if scope:
                body["scope"] = scope
            r = requests.post(
                token_url,
                data=body,
                headers={
                    "Content-Type": "application/x-www-form-urlencoded",
                    "Accept": "application/json",
                    "User-Agent": "Dr3shLauncher",
                },
                timeout=15,
            )
            if not r.text:
                continue
            token_data = r.json()
            ms_access_token = token_data.get("access_token")
            if not ms_access_token:
                if token_data.get("error_description"):
                    last_exc = LaunchError(token_data.get("error_description"))
                continue
            next_refresh = token_data.get("refresh_token") or refresh_token
            return complete_microsoft_login(ms_access_token, next_refresh, include_access_token=True)
        except Exception as exc:
            last_exc = exc
            continue

    if client_id == "00000000402b5328":
        try:
            profile = mll_mod.microsoft_account.complete_refresh(client_id, None, None, refresh_token)
            if isinstance(profile, dict) and profile.get("access_token") and profile.get("id") and profile.get("name"):
                return profile
        except Exception as exc:
            last_exc = exc

    if last_exc:
        raise last_exc
    raise LaunchError("No se pudo renovar la sesión de Microsoft.")


def is_minecraft_token_valid(token: str) -> bool:
    """Return True if the Minecraft JWT access token is present and unexpired."""
    if not token or not isinstance(token, str):
        return False
    parts = token.strip().split(".")
    if len(parts) != 3:
        return False
    try:
        import base64
        import json
        import time

        payload_b64 = parts[1]
        payload_b64 += "=" * (-len(payload_b64) % 4)
        payload = json.loads(base64.urlsafe_b64decode(payload_b64.encode("utf-8")))
        exp = int(payload.get("exp") or 0)
        return exp > (int(time.time()) + 300)
    except Exception:
        return False
