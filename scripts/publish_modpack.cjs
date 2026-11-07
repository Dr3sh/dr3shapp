#!/usr/bin/env node
const fs = require('fs')
const path = require('path')
const os = require('os')
const crypto = require('crypto')
const zlib = require('zlib')
const https = require('https')

const DEFAULT_RULES = [
	{ pattern: 'mods/**', policy: 'managed' },
	{ pattern: 'resourcepacks/**', policy: 'managed' },
	{ pattern: 'shaderpacks/**', policy: 'managed' },
	{ pattern: 'config/**', policy: 'update_on_change' },
	{ pattern: 'defaultconfigs/**', policy: 'update_on_change' },
	{ pattern: 'kubejs/**', policy: 'update_on_change' },
	{ pattern: 'options.txt', policy: 'seed' },
	{ pattern: 'optionsof.txt', policy: 'seed' },
	{ pattern: 'optionsshaders.txt', policy: 'seed' },
	{ pattern: 'servers.dat', policy: 'seed' },
	{ pattern: 'saves/**', policy: 'ignore' },
	{ pattern: 'screenshots/**', policy: 'ignore' },
	{ pattern: 'logs/**', policy: 'ignore' },
	{ pattern: 'crash-reports/**', policy: 'ignore' },
	{ pattern: 'backups/**', policy: 'ignore' },
	{ pattern: 'xaerowaypoints/**', policy: 'ignore' },
	{ pattern: 'xaeroworldmap/**', policy: 'ignore' },
	{ pattern: 'schematics/**', policy: 'ignore' },
	{ pattern: 'replay_recordings/**', policy: 'ignore' },
	{ pattern: 'replay_videos/**', policy: 'ignore' },
	{ pattern: '.dr3sh/**', policy: 'ignore' },
	{ pattern: '**', policy: 'update_on_change' },
]

const VALID_POLICIES = new Set(['managed', 'update_on_change', 'seed', 'ignore'])
const OS_JUNK_NAMES = new Set(['thumbs.db', 'desktop.ini', '.ds_store'])

const CRC32_TABLE = (() => {
	const table = new Uint32Array(256)
	for (let i = 0; i < 256; i++) {
		let c = i
		for (let k = 0; k < 8; k++) {
			c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
		}
		table[i] = c >>> 0
	}
	return table
})()

function crc32(buf) {
	let crc = 0xffffffff
	for (let i = 0; i < buf.length; i++) {
		crc = CRC32_TABLE[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8)
	}
	return (crc ^ 0xffffffff) >>> 0
}

function sha256Hex(buf) {
	return crypto.createHash('sha256').update(buf).digest('hex')
}

function sha256File(filePath) {
	return new Promise((resolve, reject) => {
		const hash = crypto.createHash('sha256')
		const stream = fs.createReadStream(filePath)
		stream.on('data', (chunk) => hash.update(chunk))
		stream.on('end', () => resolve(hash.digest('hex')))
		stream.on('error', reject)
	})
}

function segmentMatches(pattern, text) {
	const p = Array.from(pattern)
	const t = Array.from(text)
	let pi = 0
	let ti = 0
	let starIdx = -1
	let matchIdx = 0

	while (ti < t.length) {
		if (pi < p.length && p[pi] === t[ti]) {
			pi++
			ti++
		} else if (pi < p.length && p[pi] === '*') {
			starIdx = pi
			matchIdx = ti
			pi++
		} else if (starIdx !== -1) {
			pi = starIdx + 1
			matchIdx++
			ti = matchIdx
		} else {
			return false
		}
	}

	while (pi < p.length && p[pi] === '*') {
		pi++
	}
	return pi === p.length
}

function matchSegments(patSegs, pathSegs) {
	if (patSegs.length === 0) return pathSegs.length === 0
	if (patSegs[0] === '**') {
		const rest = patSegs.slice(1)
		if (rest.length === 0) return true
		for (let skip = 0; skip <= pathSegs.length; skip++) {
			if (matchSegments(rest, pathSegs.slice(skip))) return true
		}
		return false
	}
	if (pathSegs.length === 0) return false
	if (!segmentMatches(patSegs[0], pathSegs[0])) return false
	return matchSegments(patSegs.slice(1), pathSegs.slice(1))
}

function globMatches(pattern, relPath) {
	const patNorm = String(pattern || '')
		.trim()
		.replace(/\\/g, '/')
		.replace(/^\/+|\/+$/g, '')
		.toLowerCase()
	const pathNorm = String(relPath || '')
		.trim()
		.replace(/\\/g, '/')
		.replace(/^\/+|\/+$/g, '')
		.toLowerCase()

	if (patNorm === '**') return true
	const patSegs = patNorm ? patNorm.split('/') : []
	const pathSegs = pathNorm ? pathNorm.split('/') : []
	return matchSegments(patSegs, pathSegs)
}

function resolvePolicy(rules, relPath) {
	for (const rule of rules) {
		if (globMatches(rule.pattern, relPath)) {
			return rule.policy
		}
	}
	return 'update_on_change'
}

function validateSafeRelativePath(relPath) {
	if (!relPath || typeof relPath !== 'string') {
		throw new Error('Empty path in modpack')
	}
	if (relPath.includes('\\')) {
		throw new Error(`Backslash not allowed in path: ${relPath}`)
	}
	if (relPath.startsWith('/') || relPath.endsWith('/')) {
		throw new Error(`Leading or trailing slash not allowed: ${relPath}`)
	}
	if (relPath.includes(':')) {
		throw new Error(`Drive letter or colon not allowed: ${relPath}`)
	}
	const segs = relPath.split('/')
	for (const seg of segs) {
		if (!seg || seg === '.' || seg === '..') {
			throw new Error(`Unsafe path segment '${seg}' in: ${relPath}`)
		}
	}
}

function loadAndValidateRules(packId, customRulesPath) {
	let rules = DEFAULT_RULES
	const defaultCandidate = path.resolve(__dirname, '..', 'modpacks', packId, 'rules.json')
	const targetRulesPath = customRulesPath ? path.resolve(customRulesPath) : defaultCandidate
	if (fs.existsSync(targetRulesPath)) {
		const raw = JSON.parse(fs.readFileSync(targetRulesPath, 'utf8'))
		rules = Array.isArray(raw) ? raw : raw.rules
	}

	if (!Array.isArray(rules) || rules.length === 0) {
		throw new Error('Rules list must be a non-empty array')
	}
	for (const r of rules) {
		if (!r || typeof r.pattern !== 'string' || !r.pattern.trim()) {
			throw new Error(`Invalid rule pattern: ${JSON.stringify(r)}`)
		}
		if (!VALID_POLICIES.has(r.policy)) {
			throw new Error(`Invalid policy '${r.policy}' in rule '${r.pattern}'`)
		}
	}
	if (rules[rules.length - 1].pattern.trim() !== '**') {
		throw new Error("The last rule in rules[] MUST have pattern '**'")
	}
	return rules
}

function walkSourceDirectory(rootDir, rules) {
	const collected = []
	const seenLower = new Set()

	function walk(currentDir, relPrefix) {
		const entries = fs.readdirSync(currentDir, { withFileTypes: true })
		for (const entry of entries) {
			const nameLower = entry.name.toLowerCase()
			if (OS_JUNK_NAMES.has(nameLower)) {
				continue
			}
			const relPath = relPrefix ? `${relPrefix}/${entry.name}` : entry.name
			const fullPath = path.join(currentDir, entry.name)

			if (entry.isDirectory()) {
				const dirPolicy = resolvePolicy(rules, `${relPath}/placeholder`)
				if (dirPolicy === 'ignore' && globMatches('.dr3sh/**', `${relPath}/x`)) {
					continue
				}
				walk(fullPath, relPath)
			} else if (entry.isFile()) {
				validateSafeRelativePath(relPath)
				const policy = resolvePolicy(rules, relPath)
				if (!VALID_POLICIES.has(policy)) {
					throw new Error(`Invalid resolved policy '${policy}' for ${relPath}`)
				}
				if (policy === 'ignore') {
					continue
				}
				const lowerKey = relPath.toLowerCase()
				if (seenLower.has(lowerKey)) {
					throw new Error(`Case-insensitive duplicate path detected: ${relPath}`)
				}
				seenLower.add(lowerKey)
				const buf = fs.readFileSync(fullPath)
				collected.push({
					path: relPath,
					fullPath,
					size: buf.length,
					sha256: sha256Hex(buf),
					policy,
				})
			}
		}
	}

	walk(rootDir, '')
	collected.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
	return collected
}

function writeZipDeterministic(entries, outputZipPath) {
	const fd = fs.openSync(outputZipPath, 'w')
	let offset = 0
	const centralDirectory = []

	try {
		for (const item of entries) {
			const nameBuf = Buffer.from(item.path, 'utf8')
			const rawData = fs.readFileSync(item.fullPath)
			const uncompressedSize = rawData.length
			const fileCrc = crc32(rawData)
			const deflated = zlib.deflateRawSync(rawData, { level: 6 })

			const useDeflate = deflated.length < uncompressedSize
			const method = useDeflate ? 8 : 0
			const payload = useDeflate ? deflated : rawData
			const compressedSize = payload.length

			const localHeader = Buffer.alloc(30 + nameBuf.length)
			localHeader.writeUInt32LE(0x04034b50, 0)
			localHeader.writeUInt16LE(20, 4)
			localHeader.writeUInt16LE(0x0800, 6) // UTF-8 flag
			localHeader.writeUInt16LE(method, 8)
			localHeader.writeUInt16LE(0, 10)
			localHeader.writeUInt16LE(0x5421, 12)
			localHeader.writeUInt32LE(fileCrc, 14)
			localHeader.writeUInt32LE(compressedSize, 18)
			localHeader.writeUInt32LE(uncompressedSize, 22)
			localHeader.writeUInt16LE(nameBuf.length, 26)
			localHeader.writeUInt16LE(0, 28)
			nameBuf.copy(localHeader, 30)

			const localHeaderOffset = offset
			fs.writeSync(fd, localHeader)
			offset += localHeader.length

			fs.writeSync(fd, payload)
			offset += payload.length

			const centralHeader = Buffer.alloc(46 + nameBuf.length)
			centralHeader.writeUInt32LE(0x02014b50, 0)
			centralHeader.writeUInt16LE(20, 4)
			centralHeader.writeUInt16LE(20, 6)
			centralHeader.writeUInt16LE(0x0800, 8)
			centralHeader.writeUInt16LE(method, 10)
			centralHeader.writeUInt16LE(0, 12)
			centralHeader.writeUInt16LE(0x5421, 14)
			centralHeader.writeUInt32LE(fileCrc, 16)
			centralHeader.writeUInt32LE(compressedSize, 20)
			centralHeader.writeUInt32LE(uncompressedSize, 24)
			centralHeader.writeUInt16LE(nameBuf.length, 28)
			centralHeader.writeUInt16LE(0, 30)
			centralHeader.writeUInt16LE(0, 32)
			centralHeader.writeUInt16LE(0, 34)
			centralHeader.writeUInt16LE(0, 36)
			centralHeader.writeUInt32LE(0, 38)
			centralHeader.writeUInt32LE(localHeaderOffset, 42)
			nameBuf.copy(centralHeader, 46)

			centralDirectory.push(centralHeader)
		}

		const cdStart = offset
		for (const cd of centralDirectory) {
			fs.writeSync(fd, cd)
			offset += cd.length
		}
		const cdSize = offset - cdStart

		const eocd = Buffer.alloc(22)
		eocd.writeUInt32LE(0x06054b50, 0)
		eocd.writeUInt16LE(0, 4)
		eocd.writeUInt16LE(0, 6)
		eocd.writeUInt16LE(entries.length, 8)
		eocd.writeUInt16LE(entries.length, 10)
		eocd.writeUInt32LE(cdSize, 12)
		eocd.writeUInt32LE(cdStart, 16)
		eocd.writeUInt16LE(0, 20)
		fs.writeSync(fd, eocd)
	} finally {
		fs.closeSync(fd)
	}
}

function readZipEntriesMap(zipPath) {
	const buf = fs.readFileSync(zipPath)
	let eocdOffset = -1
	for (let i = buf.length - 22; i >= Math.max(0, buf.length - 65557); i--) {
		if (buf.readUInt32LE(i) === 0x06054b50) {
			eocdOffset = i
			break
		}
	}
	if (eocdOffset === -1) {
		throw new Error(`Invalid ZIP archive (EOCD not found): ${zipPath}`)
	}

	const totalEntries = buf.readUInt16LE(eocdOffset + 10)
	const cdOffset = buf.readUInt32LE(eocdOffset + 16)
	const entries = new Map()

	let ptr = cdOffset
	for (let i = 0; i < totalEntries; i++) {
		if (buf.readUInt32LE(ptr) !== 0x02014b50) {
			throw new Error(`Invalid Central Directory header at offset ${ptr}`)
		}
		const method = buf.readUInt16LE(ptr + 10)
		const compressedSize = buf.readUInt32LE(ptr + 20)
		const uncompressedSize = buf.readUInt32LE(ptr + 24)
		const nameLen = buf.readUInt16LE(ptr + 28)
		const extraLen = buf.readUInt16LE(ptr + 30)
		const commentLen = buf.readUInt16LE(ptr + 32)
		const localHeaderOffset = buf.readUInt32LE(ptr + 42)
		const name = buf.subarray(ptr + 46, ptr + 46 + nameLen).toString('utf8')

		ptr += 46 + nameLen + extraLen + commentLen

		if (name.endsWith('/')) continue

		const localNameLen = buf.readUInt16LE(localHeaderOffset + 26)
		const localExtraLen = buf.readUInt16LE(localHeaderOffset + 28)
		const dataStart = localHeaderOffset + 30 + localNameLen + localExtraLen
		const compSlice = buf.subarray(dataStart, dataStart + compressedSize)

		let rawData
		if (method === 0) {
			rawData = compSlice
		} else if (method === 8) {
			rawData = zlib.inflateRawSync(compSlice)
		} else {
			throw new Error(`Unsupported ZIP compression method ${method} for entry ${name}`)
		}

		if (rawData.length !== uncompressedSize) {
			throw new Error(`Uncompressed size mismatch for ${name}`)
		}
		entries.set(name, {
			size: rawData.length,
			sha256: sha256Hex(rawData),
			data: rawData,
		})
	}

	return entries
}

function extractInputZipToTempDir(zipPath) {
	const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'dr3sh-src-zip-'))
	const entries = readZipEntriesMap(zipPath)
	const rawNames = Array.from(entries.keys()).map((n) => n.replace(/\\/g, '/').replace(/^\/+/, ''))

	let commonPrefix = null
	for (const name of rawNames) {
		const slashIdx = name.indexOf('/')
		if (slashIdx === -1) {
			commonPrefix = null
			break
		}
		const candidate = name.slice(0, slashIdx + 1)
		if (commonPrefix === null) {
			commonPrefix = candidate
		} else if (commonPrefix !== candidate) {
			commonPrefix = null
			break
		}
	}

	if (commonPrefix) {
		const topFolder = commonPrefix.slice(0, -1).toLowerCase()
		const standardDirs = new Set([
			'mods',
			'config',
			'resourcepacks',
			'shaderpacks',
			'kubejs',
			'defaultconfigs',
			'saves',
			'datapacks',
		])
		if (standardDirs.has(topFolder)) {
			commonPrefix = null
		}
	}

	for (const [rawName, info] of entries.entries()) {
		let rel = rawName.replace(/\\/g, '/').replace(/^\/+/, '')
		if (commonPrefix && rel.startsWith(commonPrefix)) {
			rel = rel.slice(commonPrefix.length).replace(/^\/+/, '')
		}
		if (!rel) continue
		const dest = path.join(tempDir, rel)
		fs.mkdirSync(path.dirname(dest), { recursive: true })
		fs.writeFileSync(dest, info.data)
	}

	return tempDir
}

function selfCheckZipAgainstManifest(zipPath, manifest) {
	const zipEntries = readZipEntriesMap(zipPath)
	for (const fileEntry of manifest.files) {
		const found = zipEntries.get(fileEntry.path)
		if (!found) {
			throw new Error(`Self-check failed: manifest entry missing from ZIP: ${fileEntry.path}`)
		}
		if (found.sha256 !== fileEntry.sha256) {
			throw new Error(
				`Self-check failed: SHA-256 mismatch for ${fileEntry.path} (manifest=${fileEntry.sha256}, zip=${found.sha256})`,
			)
		}
		if (found.size !== fileEntry.size) {
			throw new Error(
				`Self-check failed: size mismatch for ${fileEntry.path} (manifest=${fileEntry.size}, zip=${found.size})`,
			)
		}
	}
}

function resolveGithubToken(explicitToken) {
	if (explicitToken && explicitToken.trim()) return explicitToken.trim()
	if (process.env.GITHUB_TOKEN && process.env.GITHUB_TOKEN.trim()) {
		return process.env.GITHUB_TOKEN.trim()
	}
	const candidates = [
		path.resolve(process.cwd(), 'active_token.txt'),
		path.resolve(process.cwd(), '..', 'active_token.txt'),
		path.resolve(__dirname, '..', 'active_token.txt'),
		path.resolve(__dirname, '..', '..', 'active_token.txt'),
	]
	for (const p of candidates) {
		if (fs.existsSync(p)) {
			const val = fs.readFileSync(p, 'utf8').trim()
			if (val) return val
		}
	}
	return ''
}

function githubApiRequest(endpoint, token, options = {}) {
	return new Promise((resolve, reject) => {
		const url = endpoint.startsWith('http')
			? new URL(endpoint)
			: new URL(`https://api.github.com${endpoint}`)
		const headers = {
			'User-Agent': 'Dr3shLauncher/1.0.0 (+https://github.com/Dr3sh/dr3shapp)',
			Authorization: `Bearer ${token}`,
			Accept: 'application/vnd.github+json',
			...(options.headers || {}),
		}
		if (
			options.body &&
			typeof options.body === 'string' &&
			!headers['Content-Length'] &&
			!headers['content-length']
		) {
			headers['Content-Length'] = Buffer.byteLength(options.body, 'utf8')
		}

		const req = https.request(
			{
				hostname: url.hostname,
				path: url.pathname + url.search,
				method: options.method || 'GET',
				headers,
			},
			(res) => {
				let data = Buffer.alloc(0)
				res.on('data', (chunk) => {
					data = Buffer.concat([data, chunk])
				})
				res.on('end', () => {
					const text = data.toString('utf8')
					try {
						const json = JSON.parse(text)
						if (res.statusCode >= 200 && res.statusCode < 300) {
							resolve(json)
						} else {
							reject(new Error(`GitHub API Error ${res.statusCode}: ${json.message || text}`))
						}
					} catch {
						if (res.statusCode >= 200 && res.statusCode < 300) {
							resolve(text)
						} else {
							reject(new Error(`GitHub HTTP ${res.statusCode}: ${text}`))
						}
					}
				})
			},
		)

		req.on('error', reject)
		if (options.body) {
			if (typeof options.body.pipe === 'function') {
				options.body.pipe(req)
			} else {
				req.write(options.body)
				req.end()
			}
		} else {
			req.end()
		}
	})
}

async function uploadReleaseAsset(uploadUrlTemplate, token, filePath, fileName) {
	const uploadUrl = uploadUrlTemplate.replace(
		/\{(\?.*)?\}$/,
		`?name=${encodeURIComponent(fileName)}`,
	)
	const fileSize = fs.statSync(filePath).size
	const fileStream = fs.createReadStream(filePath)
	return await githubApiRequest(uploadUrl, token, {
		method: 'POST',
		headers: {
			'Content-Type': fileName.endsWith('.zip') ? 'application/zip' : 'application/json',
			'Content-Length': fileSize,
		},
		body: fileStream,
	})
}

async function publishModpack(options) {
	const {
		packId,
		version,
		srcPath,
		repo = 'Dr3sh/modpack-versions',
		rulesPath = null,
		dryRun = false,
		minecraftVersion = '1.21.1',
		loader = 'fabric',
		loaderVersion = null,
		title = null,
		description = '',
		versionsFile = null,
		token: explicitToken = null,
		outDir = null,
	} = options

	if (!packId) throw new Error('--pack <pack_id> is required')
	if (!version) throw new Error('--version <semver> is required')
	if (!srcPath || !fs.existsSync(srcPath)) {
		throw new Error(`--src path does not exist: ${srcPath}`)
	}

	const cleanPackId = String(packId).trim().toLowerCase()
	const cleanVersion = String(version).trim().replace(/^v/i, '')
	const tagName = `${cleanPackId}-v${cleanVersion.replace(/[^a-zA-Z0-9.-]+/g, '-')}`
	const zipFileName = `${cleanPackId}-${cleanVersion}.zip`
	const zipDownloadUrl = `https://github.com/${repo}/releases/download/${tagName}/${zipFileName}`
	const manifestDownloadUrl = `https://github.com/${repo}/releases/download/${tagName}/manifest.json`

	const rules = loadAndValidateRules(cleanPackId, rulesPath)

	let tempExtractedDir = null
	let effectiveSrcDir = path.resolve(srcPath)
	if (fs.statSync(effectiveSrcDir).isFile() && effectiveSrcDir.toLowerCase().endsWith('.zip')) {
		tempExtractedDir = extractInputZipToTempDir(effectiveSrcDir)
		effectiveSrcDir = tempExtractedDir
	}

	const workDir = outDir
		? path.resolve(outDir)
		: fs.mkdtempSync(path.join(os.tmpdir(), `dr3sh-pack-${cleanPackId}-`))
	fs.mkdirSync(workDir, { recursive: true })

	const outputZipPath = path.join(workDir, zipFileName)
	const outputManifestPath = path.join(workDir, 'manifest.json')

	try {
		const collectedFiles = walkSourceDirectory(effectiveSrcDir, rules)
		writeZipDeterministic(collectedFiles, outputZipPath)

		const zipStat = fs.statSync(outputZipPath)
		const zipSha256 = await sha256File(outputZipPath)

		const manifest = {
			schema: 1,
			pack_id: cleanPackId,
			version: cleanVersion,
			minecraft_version: minecraftVersion,
			loader,
			...(loaderVersion ? { loader_version: loaderVersion } : {}),
			zip: {
				url: zipDownloadUrl,
				size: zipStat.size,
				sha256: zipSha256,
			},
			rules,
			files: collectedFiles.map((f) => ({
				path: f.path,
				sha256: f.sha256,
				size: f.size,
				policy: f.policy,
			})),
		}

		fs.writeFileSync(outputManifestPath, `${JSON.stringify(manifest, null, '\t')}\n`, 'utf8')

		// Step 6: Re-open the produced ZIP and verify every manifest entry exists with matching SHA-256
		selfCheckZipAgainstManifest(outputZipPath, manifest)

		const sizeMb = Math.round((zipStat.size / (1024 * 1024)) * 10) / 10
		const versionFragment = {
			id: `v${cleanVersion}`,
			name: title || `${cleanPackId} v${cleanVersion}`,
			game_version: minecraftVersion,
			loader,
			...(loaderVersion ? { loader_version: loaderVersion } : {}),
			modpack: zipDownloadUrl,
			manifest: manifestDownloadUrl,
			size: zipStat.size,
			size_mb: sizeMb,
			sha256: zipSha256,
			date: new Date().toISOString().slice(0, 10),
		}

		if (versionsFile && fs.existsSync(versionsFile)) {
			const verDoc = JSON.parse(fs.readFileSync(versionsFile, 'utf8'))
			if (verDoc.servers && verDoc.servers[cleanPackId]) {
				const srv = verDoc.servers[cleanPackId]
				srv.versions = Array.isArray(srv.versions) ? srv.versions : []
				srv.versions = srv.versions.filter((v) => v.id !== versionFragment.id)
				srv.versions.unshift(versionFragment)
				fs.writeFileSync(versionsFile, `${JSON.stringify(verDoc, null, '\t')}\n`, 'utf8')
			}
		}

		let releaseUrl = `https://github.com/${repo}/releases/tag/${tagName}`
		if (!dryRun) {
			const token = resolveGithubToken(explicitToken)
			if (!token) {
				throw new Error(
					'GitHub token not found. Set GITHUB_TOKEN env var or place token in active_token.txt.',
				)
			}

			let release
			try {
				release = await githubApiRequest(
					`/repos/${repo}/releases/tags/${encodeURIComponent(tagName)}`,
					token,
				)
			} catch {
				release = await githubApiRequest(`/repos/${repo}/releases`, token, {
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						tag_name: tagName,
						name: title || `${cleanPackId} v${cleanVersion}`,
						body: description || `Modpack release ${tagName}`,
						draft: false,
						prerelease: false,
					}),
				})
			}

			releaseUrl = release.html_url || releaseUrl
			const existingAssets = release.assets || []
			for (const assetName of [zipFileName, 'manifest.json']) {
				const match = existingAssets.find((a) => a.name === assetName)
				if (match) {
					try {
						await githubApiRequest(`/repos/${repo}/releases/assets/${match.id}`, token, {
							method: 'DELETE',
						})
					} catch {}
				}
			}

			await uploadReleaseAsset(release.upload_url, token, outputZipPath, zipFileName)
			await uploadReleaseAsset(release.upload_url, token, outputManifestPath, 'manifest.json')
		}

		return {
			tag_name: tagName,
			archive_name: zipFileName,
			release_url: releaseUrl,
			archive_url: zipDownloadUrl,
			manifest_url: manifestDownloadUrl,
			size: zipStat.size,
			size_mb: sizeMb,
			sha256: zipSha256,
			files_count: collectedFiles.length,
			output_zip_path: outputZipPath,
			output_manifest_path: outputManifestPath,
			version_fragment: versionFragment,
		}
	} finally {
		if (tempExtractedDir) {
			try {
				fs.rmSync(tempExtractedDir, { recursive: true, force: true })
			} catch {}
		}
	}
}

if (require.main === module) {
	const args = process.argv.slice(2)
	const getArg = (flag) => {
		const idx = args.indexOf(flag)
		return idx !== -1 && args[idx + 1] && !args[idx + 1].startsWith('--') ? args[idx + 1] : null
	}
	const hasFlag = (flag) => args.includes(flag)

	const packId = getArg('--pack') || getArg('--server')
	const version = getArg('--version')
	const srcPath = getArg('--src') || getArg('--source') || getArg('--zip')
	const repo = getArg('--repo') || 'Dr3sh/modpack-versions'
	const rulesPath = getArg('--rules')
	const versionsFile = getArg('--versions-file')
	const outDir = getArg('--out-dir') || getArg('--out')
	const minecraftVersion = getArg('--mc') || getArg('--mc-version') || '1.21.1'
	const loader = getArg('--loader') || 'fabric'
	const loaderVersion = getArg('--loader-version')
	const title = getArg('--title')
	const description = getArg('--description') || ''
	const dryRun = hasFlag('--dry-run')

	publishModpack({
		packId,
		version,
		srcPath,
		repo,
		rulesPath,
		dryRun,
		minecraftVersion,
		loader,
		loaderVersion,
		title,
		description,
		versionsFile,
		outDir,
	})
		.then((res) => {
			console.log(`ZIP URL:      ${res.archive_url}`)
			console.log(`Manifest URL: ${res.manifest_url}`)
			console.log(`ZIP SHA-256:  ${res.sha256}`)
			console.log(`ZIP Size:     ${res.size} bytes (${res.size_mb} MB)`)
			console.log(`Files count:  ${res.files_count}`)
			console.log('\nversions.json entry:')
			console.log(JSON.stringify(res.version_fragment, null, '\t'))
			console.log(`DR3SH_RESULT_JSON_START${JSON.stringify(res)}DR3SH_RESULT_JSON_END`)
		})
		.catch((err) => {
			console.error(err.message || String(err))
			process.exit(1)
		})
}

module.exports = {
	publishModpack,
	DEFAULT_RULES,
	globMatches,
	resolvePolicy,
}
