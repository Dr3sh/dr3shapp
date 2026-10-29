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
const REPO = getArg('--repo', 'Dr3sh-App');
const rawVer = getArg('--version', pkg.version || '0.2.16');
const TAG = rawVer.startsWith('v') ? rawVer : `v${rawVer}`;
const TITLE = `Dr3sh Launcher ${TAG}`;

let TOKEN = process.env.GITHUB_TOKEN;
const activeTokenPath = path.resolve(__dirname, '../active_token.txt');
if (!TOKEN && fs.existsSync(activeTokenPath)) {
  TOKEN = fs.readFileSync(activeTokenPath, 'utf8').trim();
}
if (!TOKEN) {
  TOKEN = process.env.GH_TOKEN || '';
}

const NOTES = `## Dr3sh Launcher v${pkg.version} - Nueva Fuente UI (Plus Jakarta Sans) & Limpieza de Mods

### Novedades y Mejoras:
- **Nueva Tipografía Oficial del Launcher (Plus Jakarta Sans):**
  - Reemplazo completo de la fuente anterior por **Plus Jakarta Sans**, una fuente moderna, elegante y geométrica idéntica en estética y legibilidad a Rubik.
  - **Soporte Unicode y Glifos Completo:** Incluye soporte total para todos los caracteres del español (\`á\`, \`é\`, \`í\`, \`ó\`, \`ú\`, \`ñ\`, \`ü\`, \`¿\`, \`¡\`), caracteres especiales, signos de puntuación (\`•\`, \`·\`, \`—\`, \`–\`), flechas (\`→\`, \`←\`) y símbolos, eliminando cualquier caracter roto o rombo con signo de interrogación.
  - Empaquetada localmente en TTF de alta fidelidad sin dependencias de red externas.

- **Desactivación Completa de Dr3shSkins:**
  - El launcher ya no intenta inyectar ni descargar el mod \`dr3shskins\` por su cuenta.
  - Purga automática de cualquier archivo remanente de \`dr3shskins\` en las carpetas de mods.
  - Soporte universal de skins enfocado en \`CustomSkinLoader\`.

- **Motor de Arranque Optimizado (Prism Launcher):**
  - Banderas de memoria \`-XX:+AlwaysPreTouch\` y recolección de basura G1GC de baja latencia integradas.
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
    path.resolve(__dirname, '../Dr3sh app/apps/app/target/release/bundle/nsis'),
    path.resolve(__dirname, '../Dr3sh app/target/release/bundle/nsis'),
    path.resolve(__dirname, '../release')
  ];

  let releaseDir = null;
  for (const dir of candidateDirs) {
    if (fs.existsSync(dir)) {
      releaseDir = dir;
      break;
    }
  }

  if (!releaseDir) {
    releaseDir = path.resolve(__dirname, '../release');
  }

  // Look for any setup executable in releaseDir or candidate dirs
  let sourceExe = null;
  const filesInDir = fs.existsSync(releaseDir) ? fs.readdirSync(releaseDir) : [];
  for (const file of filesInDir) {
    if (file.toLowerCase().endsWith('.exe') && (file.toLowerCase().includes('setup') || file.toLowerCase().includes('dr3sh'))) {
      sourceExe = path.join(releaseDir, file);
      break;
    }
  }

  const assetsToUpload = [];
  if (sourceExe && fs.existsSync(sourceExe)) {
    const exeName = `Dr3sh-Launcher-Setup-${version}.exe`;
    assetsToUpload.push({ file: sourceExe, name: exeName });
  }

  // Tauri v2 updater assets
  for (const dir of candidateDirs) {
    if (!fs.existsSync(dir)) continue;
    const items = fs.readdirSync(dir);
    for (const item of items) {
      const full = path.join(dir, item);
      if (item === 'latest.json') {
        assetsToUpload.push({ file: full, name: 'latest.json' });
      } else if (item.endsWith('.nsis.zip')) {
        assetsToUpload.push({ file: full, name: item });
      } else if (item.endsWith('.nsis.zip.sig')) {
        assetsToUpload.push({ file: full, name: item });
      }
    }
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

  const existingAssets = release.assets || [];

  for (const item of assetsToUpload) {
    const oldAsset = existingAssets.find(a => a.name === item.name);
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
