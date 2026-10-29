#!/usr/bin/env node
const fs = require('fs');
const path = require('path');
const https = require('https');
const { spawn } = require('child_process');

function slug(value) {
  return String(value || '')
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '');
}

function releaseTag(serverId, version) {
  const s = slug(serverId);
  const v = slug(version.replace(/^v/i, ''));
  return `${s}-v${v}`;
}

function apiRequest(endpoint, token, options = {}) {
  return new Promise((resolve, reject) => {
    const url = endpoint.startsWith('http') ? new URL(endpoint) : new URL(`https://api.github.com${endpoint}`);
    const headers = {
      'User-Agent': 'Dr3shLauncher',
      'Authorization': `Bearer ${token}`,
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
            reject(new Error(`GitHub API Error ${res.statusCode}: ${json.message || text}`));
          }
        } catch {
          if (res.statusCode >= 200 && res.statusCode < 300) {
            resolve(text);
          } else {
            reject(new Error(`GitHub HTTP ${res.statusCode}: ${text}`));
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

async function uploadAsset(uploadUrlTemplate, token, filePath, fileName, onProgress) {
  const uploadUrl = uploadUrlTemplate.replace(/\{(\?.*)?\}$/, `?name=${encodeURIComponent(fileName)}`);
  const fileSize = fs.statSync(filePath).size;

  return new Promise((resolve, reject) => {
    const url = new URL(uploadUrl);
    const req = https.request({
      hostname: url.hostname,
      path: url.pathname + url.search,
      method: 'POST',
      headers: {
        'User-Agent': 'Dr3shLauncher',
        'Authorization': `Bearer ${token}`,
        'Content-Type': fileName.endsWith('.zip') ? 'application/zip' : 'application/json',
        'Content-Length': fileSize
      }
    }, (res) => {
      let data = Buffer.alloc(0);
      res.on('data', (chunk) => {
        data = Buffer.concat([data, chunk]);
      });
      res.on('end', () => {
        if (res.statusCode >= 200 && res.statusCode < 300) {
          try {
            resolve(JSON.parse(data.toString('utf8')));
          } catch {
            resolve(data.toString('utf8'));
          }
        } else {
          reject(new Error(`Error subiendo ${fileName} (${res.statusCode}): ${data.toString('utf8')}`));
        }
      });
    });

    req.on('error', reject);

    let uploaded = 0;
    const fileStream = fs.createReadStream(filePath);
    fileStream.on('data', (chunk) => {
      uploaded += chunk.length;
      if (onProgress) {
        onProgress({ fileName, uploaded, total: fileSize, percent: Math.round((uploaded / fileSize) * 100) });
      }
    });
    fileStream.on('error', reject);
    fileStream.pipe(req);
  });
}

function runPython(scriptPath, args) {
  return new Promise((resolve, reject) => {
    const pythonExe = fs.existsSync(path.resolve(__dirname, '../python-runtime/python.exe'))
      ? path.resolve(__dirname, '../python-runtime/python.exe')
      : 'python';

    const child = spawn(pythonExe, [scriptPath, ...args], {
      cwd: path.resolve(__dirname, '..'),
      stdio: ['ignore', 'pipe', 'pipe']
    });

    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (c) => { stdout += c.toString('utf8'); });
    child.stderr.on('data', (c) => { stderr += c.toString('utf8'); });

    child.on('close', (code) => {
      if (code === 0) {
        resolve(stdout);
      } else {
        reject(new Error(`generate_manifest.py failed (code ${code}): ${stderr || stdout}`));
      }
    });

    child.on('error', reject);
  });
}

async function publishRelease({
  token,
  serverId,
  version,
  title,
  archivePath,
  description = '',
  targetRepo = 'Dr3sh/modpack-versions',
  onProgress = () => {}
}) {
  if (!token) throw new Error('Token de GitHub requerido.');
  if (!archivePath || !fs.existsSync(archivePath)) {
    throw new Error(`Archivo ZIP no encontrado en: ${archivePath}`);
  }

  const cleanVer = String(version || '').replace(/^v/i, '').trim();
  const tagName = releaseTag(serverId, cleanVer);
  const archiveName = `${slug(serverId)}-${slug(cleanVer)}.zip`;
  const archiveStat = fs.statSync(archivePath);
  const sizeMb = Math.round((archiveStat.size / (1024 * 1024)) * 10) / 10;

  // Step 1: Generate manifest.json with Python
  onProgress({ phase: 'manifest', message: 'Generando manifest.json con Python...' });
  const tempDir = fs.mkdtempSync(path.join(require('os').tmpdir(), 'dr3sh-manifest-'));
  const manifestPath = path.join(tempDir, 'manifest.json');
  const manifestScript = path.resolve(__dirname, 'generate_manifest.py');

  await runPython(manifestScript, [
    '--new', archivePath,
    '--version', cleanVer,
    '--output', manifestPath
  ]);

  if (!fs.existsSync(manifestPath)) {
    throw new Error('No se pudo generar el archivo manifest.json.');
  }

  const manifestData = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  const sha256 = manifestData.archive?.sha256 || '';

  // Step 2: Create or retrieve GitHub Release
  onProgress({ phase: 'release', message: `Creando release ${tagName} en ${targetRepo}...` });
  let release;
  try {
    release = await apiRequest(`/repos/${targetRepo}/releases/tags/${encodeURIComponent(tagName)}`, token);
  } catch {
    release = await apiRequest(`/repos/${targetRepo}/releases`, token, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        tag_name: tagName,
        name: title || `${serverId} v${cleanVer}`,
        body: description || `Modpack release ${tagName}`,
        draft: false,
        prerelease: false
      })
    });
  }

  // Remove existing assets with identical names if any
  const existingAssets = release.assets || [];
  for (const assetName of [archiveName, 'manifest.json']) {
    const existing = existingAssets.find((a) => a.name === assetName);
    if (existing) {
      try {
        await apiRequest(`/repos/${targetRepo}/releases/assets/${existing.id}`, token, { method: 'DELETE' });
      } catch {}
    }
  }

  // Step 3: Upload ZIP
  onProgress({ phase: 'upload-zip', message: `Subiendo ${archiveName} (${sizeMb} MB)...` });
  await uploadAsset(release.upload_url, token, archivePath, archiveName, (p) => {
    onProgress({ phase: 'upload-zip', ...p, message: `Subiendo ZIP: ${p.percent}% (${(p.uploaded / (1024*1024)).toFixed(1)} MB)` });
  });

  // Step 4: Upload manifest.json
  onProgress({ phase: 'upload-manifest', message: 'Subiendo manifest.json a la release...' });
  await uploadAsset(release.upload_url, token, manifestPath, 'manifest.json');

  // Cleanup temp dir
  try {
    fs.rmSync(tempDir, { recursive: true, force: true });
  } catch {}

  const archiveUrl = `https://github.com/${targetRepo}/releases/download/${encodeURIComponent(tagName)}/${encodeURIComponent(archiveName)}`;
  const manifestUrl = `https://github.com/${targetRepo}/releases/download/${encodeURIComponent(tagName)}/manifest.json`;

  return {
    tag_name: tagName,
    archive_name: archiveName,
    release_url: release.html_url,
    archive_url: archiveUrl,
    manifest_url: manifestUrl,
    sha256: sha256,
    size_mb: sizeMb
  };
}

module.exports = {
  publishRelease,
  releaseTag,
  slug
};

// CLI execution if run directly
if (require.main === module) {
  const args = process.argv.slice(2);
  let serverId = '';
  let version = '';
  let archivePath = '';
  let token = process.env.GITHUB_TOKEN || '';
  let title = '';
  let description = '';

  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--server') serverId = args[++i];
    else if (args[i] === '--version') version = args[++i];
    else if (args[i] === '--zip') archivePath = args[++i];
    else if (args[i] === '--token') token = args[++i];
    else if (args[i] === '--title') title = args[++i];
    else if (args[i] === '--description') description = args[++i];
  }

  if (!serverId || !version || !archivePath) {
    console.error('Uso: node publish_release.cjs --server <id> --version <ver> --zip <path> [--token <gh_token>] [--title <title>] [--description <desc>]');
    process.exit(1);
  }

  publishRelease({
    token,
    serverId,
    version,
    title,
    archivePath,
    description,
    onProgress: (p) => {
      const msg = typeof p === 'string' ? p : (p && p.message ? p.message : null);
      if (msg) console.error(msg);
    }
  }).then((res) => {
    console.error('\n¡Release publicada exitosamente!');
    console.log('DR3SH_RESULT_JSON_START' + JSON.stringify(res) + 'DR3SH_RESULT_JSON_END');
  }).catch((err) => {
    console.error('\nError publicando release:', err.message);
    process.exit(1);
  });
}
