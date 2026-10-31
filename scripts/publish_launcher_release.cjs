const fs = require('fs');
const path = require('path');
const https = require('https');

let pkg = {};
try {
  pkg = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../package.json'), 'utf8'));
} catch {
  pkg = { version: '0.2.16' };
}

// Parse optional CLI flags: --version <ver> --repo <repo>
const args = process.argv.slice(2);
function getArg(flag, fallback) {
  const idx = args.indexOf(flag);
  return idx !== -1 && args[idx + 1] ? args[idx + 1] : fallback;
}

const OWNER = 'Dr3sh';
const REPO = getArg('--repo', 'dr3shapp');
const rawVer = getArg('--version', '1.0.0');
const TAG = rawVer.startsWith('v') ? rawVer : `v${rawVer}`;
const TITLE = `Dr3sh Launcher ${TAG} (Tauri 2.0)`;

let TOKEN = process.env.GITHUB_TOKEN;
const candidateTokenPaths = [
  path.resolve(__dirname, '../active_token.txt'),
  path.resolve(__dirname, '../../active_token.txt')
];
for (const p of candidateTokenPaths) {
  if (!TOKEN && fs.existsSync(p)) {
    TOKEN = fs.readFileSync(p, 'utf8').trim();
    break;
  }
}
if (!TOKEN) {
  TOKEN = process.env.GH_TOKEN || '';
}

const NOTES = `## Dr3sh Launcher ${TAG} - Motor Nativo Tauri 2.0 & Rust

### Novedades y Mejoras:
- **Nuevo Motor Nativo con Tauri 2.0 & Rust:**
  - Rendimiento ultrarrápido con menor consumo de RAM (~40-60 MB vs +300 MB en Electron).
  - Tamaño del instalador optimizado a **26.6 MB**.
- **Instalador NSIS con Desinstalación Segura:**
  - Desinstalador limpio y seguro para el usuario, sin afectar carpetas de otros entornos o launchers.
- **Onboarding Interactivo en Primer Inicio:**
  - Modal automático en el primer lanzamiento para configurar cuenta Microsoft o cuenta Offline ("Player" personalizable).
- **Correcciones en Descargas e Interfaz:**
  - Solucionado el congelamiento de descarga al 2% mediante sincronización reactiva en Rust.
  - Menú de 3 puntos corregido con z-index aislado para evitar superposición con barras fijas de descarga.
- **Modo Mantenimiento y Modo Testing Integrados:**
  - Validación de estado en el backend para prevenir bypass por frontend o consola.
  - Purga automática de instancias de prueba tras cumplirse las horas asignadas de inactividad.
- **Descargas Concurrentes de Alta Velocidad:**
  - Motor de descarga asíncrono en Rust optimizado para descargas masivas y enlaces NTFS transparentes.
- **Sistema de Auto-actualizaciones Criptográfico:**
  - Verificación de integridad con claves minisign y endpoint de actualización oficial.
`;

function apiRequest(endpoint, options = {}) {
  return new Promise((resolve, reject) => {
    const isUpload = endpoint.startsWith('https://uploads.github.com');
    const url = endpoint.startsWith('http') ? new URL(endpoint) : new URL(`https://api.github.com${endpoint}`);
    const headers = {
      'User-Agent': 'Dr3shLauncher',
      'Authorization': `Bearer ${TOKEN}`,
      'Accept': 'application/vnd.github+json',
      ...(options.headers || {})
    };

    if (options.body && typeof options.body === 'string' && !headers['Content-Length'] && !headers['content-length']) {
      headers['Content-Length'] = Buffer.byteLength(options.body, 'utf8');
    }

    const reqOptions = {
      hostname: url.hostname,
      path: url.pathname + url.search,
      method: options.method || 'GET',
      headers
    };

    const req = https.request(reqOptions, (res) => {
      let data = Buffer.alloc(0);
      res.on('data', (chunk) => {
        data = Buffer.concat([data, chunk]);
      });
      res.on('end', () => {
        const text = data.toString('utf8');
        try {
          const json = JSON.parse(text);
          if (res.statusCode >= 200 && res.statusCode < 300) {
            resolve(json);
          } else {
            reject(new Error(`API Error ${res.statusCode}: ${JSON.stringify(json)}`));
          }
        } catch {
          if (res.statusCode >= 200 && res.statusCode < 300) {
            resolve(text);
          } else {
            reject(new Error(`HTTP ${res.statusCode}: ${text}`));
          }
        }
      });
    });

    req.on('error', reject);
    if (options.body) {
      if (typeof options.body.pipe === 'function') {
        options.body.pipe(req);
      } else {
        req.write(options.body);
        req.end();
      }
    } else {
      req.end();
    }
  });
}

async function uploadAsset(uploadUrlTemplate, filePath, fileName) {
  const uploadUrl = uploadUrlTemplate.replace(/\{(\?.*)?\}$/, `?name=${encodeURIComponent(fileName)}`);
  const fileSize = fs.statSync(filePath).size;
  console.log(`Subiendo ${fileName} (${(fileSize / (1024 * 1024)).toFixed(1)} MB)...`);

  const fileStream = fs.createReadStream(filePath);
  return apiRequest(uploadUrl, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/octet-stream',
      'Content-Length': fileSize
    },
    body: fileStream
  });
}

async function main() {
  const version = TAG.replace(/^v/, '');
  const candidateDirs = [
    'E:/cargo-target-dr3shapp/release/bundle/nsis',
    'E:/cargo-target-dr3shapp/release',
    path.resolve(__dirname, '../Dr3sh app/apps/app/target/release/bundle/nsis'),
    path.resolve(__dirname, '../Dr3sh app/target/release/bundle/nsis'),
    path.resolve(__dirname, '../release'),
    path.resolve(__dirname, '..')
  ];

  let sourceExe = null;
  let sourceSig = null;

  for (const dir of candidateDirs) {
    if (!fs.existsSync(dir)) continue;
    const items = fs.readdirSync(dir);
    for (const item of items) {
      if (item.toLowerCase().endsWith('.exe') && (item.toLowerCase().includes('setup') || item.toLowerCase().includes('dr3sh'))) {
        if (!sourceExe) sourceExe = path.join(dir, item);
      }
      if (item.toLowerCase().endsWith('.sig')) {
        if (!sourceSig) sourceSig = path.join(dir, item);
      }
    }
  }

  const assetsToUpload = [];
  if (sourceExe && fs.existsSync(sourceExe)) {
    const friendlyName = `Dr3sh-Launcher-Setup-${version}.exe`;
    const originalName = path.basename(sourceExe);
    assetsToUpload.push({ file: sourceExe, name: friendlyName });
    if (originalName !== friendlyName) {
      assetsToUpload.push({ file: sourceExe, name: originalName });
    }
  }

  if (sourceSig && fs.existsSync(sourceSig)) {
    const originalSigName = path.basename(sourceSig);
    assetsToUpload.push({ file: sourceSig, name: originalSigName });

    // Generate latest.json for Tauri updater
    const sigContent = fs.readFileSync(sourceSig, 'utf8').trim();
    const latestJsonPath = path.resolve(__dirname, '../latest.json');
    const latestJsonData = {
      version: "1.0.0-local",
      notes: `Dr3sh Launcher v${version}`,
      pub_date: new Date().toISOString(),
      platforms: {
        "windows-x86_64": {
          signature: sigContent,
          url: `https://github.com/${OWNER}/${REPO}/releases/download/${TAG}/Dr3sh-Launcher-Setup-${version}.exe`
        }
      }
    };
    fs.writeFileSync(latestJsonPath, JSON.stringify(latestJsonData, null, 2), 'utf8');
    assetsToUpload.push({ file: latestJsonPath, name: 'latest.json' });
  }

  if (assetsToUpload.length === 0) {
    console.warn(`Aviso: No se encontraron instaladores compilados en los directorios de salida de Tauri o release.`);
  }

  console.log(`Buscando release existente ${TAG} en ${OWNER}/${REPO}...`);
  let release;
  try {
    release = await apiRequest(`/repos/${OWNER}/${REPO}/releases/tags/${TAG}`);
    console.log(`Release encontrada (ID: ${release.id}). Actualizando información...`);
    release = await apiRequest(`/repos/${OWNER}/${REPO}/releases/${release.id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        tag_name: TAG,
        name: TITLE,
        body: NOTES,
        draft: false,
        prerelease: false
      })
    });
  } catch (err) {
    console.log(`Creando nueva release ${TAG} en ${OWNER}/${REPO}...`);
    release = await apiRequest(`/repos/${OWNER}/${REPO}/releases`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        tag_name: TAG,
        name: TITLE,
        body: NOTES,
        draft: false,
        prerelease: false
      })
    });
  }

  console.log(`Release lista: ${release.html_url}`);

  let currentAssets = [];
  try {
    currentAssets = await apiRequest(`/repos/${OWNER}/${REPO}/releases/${release.id}/assets`);
  } catch (err) {
    currentAssets = release.assets || [];
  }

  for (const item of assetsToUpload) {
    const normalizedItemName = item.name.replace(/\s+/g, '.');
    const oldAsset = currentAssets.find(a => 
      a.name === item.name || 
      a.name === normalizedItemName || 
      a.name.replace(/\s+/g, '.') === normalizedItemName
    );
    if (oldAsset) {
      console.log(`Eliminando asset previo ${oldAsset.name} (ID: ${oldAsset.id})...`);
      try {
        await apiRequest(`/repos/${OWNER}/${REPO}/releases/assets/${oldAsset.id}`, {
          method: 'DELETE'
        });
      } catch (e) {
        console.warn(`No se pudo eliminar asset previo ${oldAsset.name}:`, e.message);
      }
    }
    await uploadAsset(release.upload_url, item.file, item.name);
  }

  console.log('¡Actualización con electron-updater publicada con éxito en GitHub!');
}

main().catch(err => {
  console.error('Error publicando release:', err);
  process.exit(1);
});
