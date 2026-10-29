"""Tiny i18n: every UI string goes through t(). Spanish is the default."""
from __future__ import annotations

_LANG = "es"

STRINGS = {
    "es": {
        "marketplace": "Marketplace", "settings": "Ajustes", "home": "Inicio",
        "empty_title": "Aún no tienes modpacks", "empty_sub": "Ve al Marketplace para obtener uno",
        "go_market": "Ir al Marketplace",
        "play": "JUGAR", "download": "Descargar", "update": "Actualizar",
        "install_selected": "Instalar versión seleccionada", "running": "Jugando…",
        "cancel": "Cancelar", "get": "Obtener", "obtained": "Obtenido ✓", "remove": "Quitar",
        "search": "Buscar…", "refresh_list": "Refrescar",
        "mc_version": "Minecraft", "loader": "Mod loader", "installed_version": "Instalada",
        "size": "Tamaño", "date": "Fecha", "changelog": "Cambios de esta versión",
        "install_mode": "Modo de instalación",
        "mode_instance": "Instancia separada", "mode_default": "Carpeta .minecraft",
        "mode_hint": "Instancia separada: cada servidor tiene su carpeta propia (recomendado). "
                     ".minecraft: el launcher cambia los archivos del modpack automáticamente.",
        "open_folder": "Abrir carpeta", "uninstall": "Desinstalar",
        "latest": "Última", "installed_tag": "Instalada",
        "downgrade_confirm": "Vas a instalar una versión más antigua que la actual.\n"
                             "Esto puede romper tus mundos. ¿Continuar?",
        "uninstall_confirm": "¿Desinstalar los archivos del modpack? (tus mundos no se tocan)",
        "added_toast": "Añadido a tu biblioteca. Ve a Inicio para descargarlo.",
        "offline": "Sin conexión: usando datos en caché",
        "update_available": "Nueva actualización para {name}",
        "screenshots": "Imágenes", "version": "Versión",
        "s_default_mode": "Modo de instalación por defecto", "s_instances_dir": "Carpeta de instancias",
        "browse": "Examinar", "s_ram": "Memoria RAM", "s_username": "Nombre de usuario (offline)",
        "s_intro": "Mostrar animación de inicio", "s_language": "Idioma (requiere reiniciar)",
        "s_json_url": "URL del JSON (avanzado)", "s_assets_url": "URL base de imágenes (avanzado)",
        "s_save": "Guardar", "s_saved": "Ajustes guardados",
        "s_open_data": "Abrir carpeta del launcher", "s_clear_cache": "Borrar caché",
        "st_preparing": "Preparando…", "st_downloading": "Descargando…",
        "st_extracting": "Extrayendo…", "st_applying": "Aplicando archivos…",
        "st_done": "Listo", "st_launching": "Iniciando Minecraft…",
        "installed_ok": "{name} instalado", "cancelled": "Cancelado",
        "err": "Error: {msg}", "startup_error": "No se pudo iniciar el launcher",
        "retry": "Reintentar", "exit": "Salir", "no_results": "Sin resultados",
        "market_sub": "Obtén modpacks y luego descárgalos desde Inicio",
        "no_versions": "Sin versiones disponibles",
    },
    "en": {
        "marketplace": "Marketplace", "settings": "Settings", "home": "Home",
        "empty_title": "You have no modpacks yet", "empty_sub": "Go to the Marketplace to get one",
        "go_market": "Go to Marketplace",
        "play": "PLAY", "download": "Download", "update": "Update",
        "install_selected": "Install selected version", "running": "Running…",
        "cancel": "Cancel", "get": "Get", "obtained": "Obtained ✓", "remove": "Remove",
        "search": "Search…", "refresh_list": "Refresh",
        "mc_version": "Minecraft", "loader": "Mod loader", "installed_version": "Installed",
        "size": "Size", "date": "Date", "changelog": "Changes in this version",
        "install_mode": "Install mode",
        "mode_instance": "Separate instance", "mode_default": ".minecraft folder",
        "mode_hint": "Separate instance: each server has its own folder (recommended). "
                     ".minecraft: the launcher swaps the modpack files automatically.",
        "open_folder": "Open folder", "uninstall": "Uninstall",
        "latest": "Latest", "installed_tag": "Installed",
        "downgrade_confirm": "You are about to install an older version than the current one.\n"
                             "This may break your worlds. Continue?",
        "uninstall_confirm": "Uninstall the modpack files? (your worlds are not touched)",
        "added_toast": "Added to your library. Go to Home to download it.",
        "offline": "Offline: using cached data",
        "update_available": "New update available for {name}",
        "screenshots": "Images", "version": "Version",
        "s_default_mode": "Default install mode", "s_instances_dir": "Instances folder",
        "browse": "Browse", "s_ram": "RAM", "s_username": "Username (offline)",
        "s_intro": "Show intro animation", "s_language": "Language (restart required)",
        "s_json_url": "JSON URL (advanced)", "s_assets_url": "Images base URL (advanced)",
        "s_save": "Save", "s_saved": "Settings saved",
        "s_open_data": "Open launcher folder", "s_clear_cache": "Clear cache",
        "st_preparing": "Preparing…", "st_downloading": "Downloading…",
        "st_extracting": "Extracting…", "st_applying": "Applying files…",
        "st_done": "Done", "st_launching": "Starting Minecraft…",
        "installed_ok": "{name} installed", "cancelled": "Cancelled",
        "err": "Error: {msg}", "startup_error": "The launcher could not start",
        "retry": "Retry", "exit": "Exit", "no_results": "No results",
        "market_sub": "Get modpacks, then download them from Home",
        "no_versions": "No versions available",
    },
}


def set_language(lang: str) -> None:
    global _LANG
    _LANG = lang if lang in STRINGS else "es"


def t(key: str, **kw) -> str:
    text = STRINGS.get(_LANG, {}).get(key) or STRINGS["en"].get(key) or key
    return text.format(**kw) if kw else text
