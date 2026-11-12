<picture>
	<source media="(prefers-color-scheme: light)" srcset=".github/assets/monorepo_cover_light.png">
	<img alt="Dr3sh Launcher Cover" src=".github/assets/monorepo_cover.png">
</picture>

<p align="center">
	<a href="https://github.com/Dr3sh/dr3shapp/releases"><img src="https://img.shields.io/github/v/release/Dr3sh/dr3shapp?color=388bfd&style=for-the-badge&label=Release" alt="Release"></a>
	<a href="https://github.com/Dr3sh/dr3shapp/issues"><img src="https://img.shields.io/github/issues-raw/Dr3sh/dr3shapp?color=a371f7&style=for-the-badge&label=Issues" alt="Issues"></a>
	<a href="https://github.com/Dr3sh/dr3shapp"><img src="https://img.shields.io/badge/Platform-Windows%20x64-0078D6?style=for-the-badge&logo=windows" alt="Platform"></a>
	<a href="https://github.com/Dr3sh/dr3shapp"><img src="https://img.shields.io/badge/Engine-Tauri%202.0%20%7C%20Rust-e05d44?style=for-the-badge" alt="Engine"></a>
	<a href="https://github.com/Dr3sh/dr3shapp"><img src="https://img.shields.io/badge/Frontend-Vue%203%20%7C%20Tailwind-42b883?style=for-the-badge" alt="Frontend"></a>
</p>

# Dr3sh Launcher

Lanzador de escritorio de alto rendimiento para **Minecraft** y modpacks de la comunidad (como **Cobbleverse**), construido sobre una arquitectura moderna con **Tauri v2**, **Vue 3** y un motor de sistema transaccional en **Rust**.

Diseñado para proporcionar una experiencia de instalación rápida, blindada frente a cortes de red o caídas del sistema, y totalmente respetuosa con las configuraciones y partidas de los jugadores.

---

## Características Principales

- **Descargas Resilientes con Reanudación Automática:**
  - Descargas segmentadas por rangos HTTP (`206 Partial Content`). Si la conexión se interrumpe, se reanuda desde el último byte descargado.
  - Validación de *Magic Bytes* (rechazo de portales cautivos o páginas de error HTML).
  - Verificación continua de integridad por hash SHA-256.
  - Sistema de reintentos automáticos con retroceso exponencial (`backoff`) y mitigación de errores `504 Gateway Time-out` de GitHub Releases con fallback automático.

- **Motor Transaccional Seguro:**
  - Las actualizaciones se preparan en un directorio de *staging* aislado y se aplican mediante un intercambio atómico (*atomic swap*).
  - En caso de apagado repentino o fallo de energía, el launcher detecta journals incompletos y ejecuta un *rollback* automático al arrancar, evitando instancias corruptas.
  - Políticas de preservación de archivos de usuario (`saves/`, `screenshots/`, `options.txt`, `servers.dat`).

- **Almacén Global Compartido:**
  - Los archivos base de Minecraft (assets, sonidos, texturas, librerías del loader y `client.jar`) se descargan una sola vez y se comparten globalmente entre todos los modpacks, reduciendo drásticamente el uso de disco y el ancho de banda.

- **Integración Nativa con Windows:**
  - Soporte de enlaces de unión NTFS (*Junction points*) seguros que impiden la eliminación accidental de directorios de destino.
  - Preflight con verificación previa de permisos y espacio disponible en disco antes de comenzar cualquier escritura.

---

## Arquitectura del Proyecto

Este monorepo utiliza **Turborepo** y **pnpm workspaces**:

```text
├── apps/
│   ├── app/             # Shell nativo de escritorio (Tauri v2 + Rust)
│   └── app-frontend/    # Interfaz de usuario (Vue 3, TypeScript, Tailwind CSS)
├── packages/
│   ├── app-lib/         # Librería central de servicios de launcher y autenticación
│   ├── api-client/      # Cliente API tipado
│   ├── assets/          # Recursos gráficos y fuentes del sistema
│   └── ui/              # Biblioteca de componentes Vue compartidos
└── scripts/             # Scripts de empaquetado, publicación y validación determinista
```

---

## Desarrollo Local

### Requisitos Previos

- **Node.js** >= 20.x y **pnpm** >= 9.x
- **Rust** (edición 2024 / MSRV 1.85+)
- **JDK 21** (Eclipse Adoptium)
- **Herramientas de compilación de Windows** (Visual Studio C++ / LLVM)

### Comandos de Desarrollo

```bash
# Instalar dependencias
pnpm install

# Iniciar la interfaz en modo desarrollo con HMR
pnpm app:dev

# Ejecutar las pruebas unitarias y simulaciones de red
cargo test -p theseus_gui --bin theseus_gui

# Verificación de compilación del backend
cargo check -p theseus_gui
```

---

## Licencia

Distribuido bajo la licencia GPL-3.0. Consulta los archivos correspondientes en el repositorio para más detalles sobre términos y condiciones.
