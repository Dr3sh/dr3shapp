#!/usr/bin/env node
const fs = require('fs')
const path = require('path')
const os = require('os')
const crypto = require('crypto')
const zlib = require('zlib')
const https = require('https')
const http = require('http')

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
const PRECOMPRESSED_EXTS = new Set([
	'.jar',
	'.zip',
	'.mrpack',
	'.png',
	'.jpg',
	'.jpeg',
	'.webp',
	'.gif',
	'.ogg',
	'.mp3',
	'.wav',
	'.flac',
	'.mp4',
	'.mkv',
	'.webm',
	'.gz',
	'.bz2',
	'.xz',
	'.zst',
	'.7z',
	'.rar',
	'.mca',
	'.nbt',
])

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
	if (typeof zlib.crc32 === 'function') {
		return zlib.crc32(buf) >>> 0
	}
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
		const stream = fs.createReadStream(filePath, { highWaterMark: 4 * 1024 * 1024 })
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

async function collectFromSources(sources, rules, emitProgress = null) {
	const pendingMap = new Map()

	for (const src of sources) {
		const resolved = path.resolve(src)
		if (!fs.existsSync(resolved)) {
			console.warn(`Origen no encontrado, omitiendo: ${resolved}`)
			continue
		}
		const stat = fs.statSync(resolved)

		if (stat.isFile()) {
			const fileName = path.basename(resolved)
			const nameLower = fileName.toLowerCase()
			if (OS_JUNK_NAMES.has(nameLower)) continue

			const relPath = fileName
			validateSafeRelativePath(relPath)
			const policy = resolvePolicy(rules, relPath)
			if (policy === 'ignore') continue

			pendingMap.set(relPath.toLowerCase(), {
				path: relPath,
				fullPath: resolved,
				size: stat.size,
				policy,
			})
		} else if (stat.isDirectory()) {
			const folderName = path.basename(resolved)
			let isInstanceRoot = false
			if (!STANDARD_MODPACK_TOP_DIRS.has(folderName.toLowerCase())) {
				try {
					const children = fs.readdirSync(resolved)
					const childrenLower = new Set(children.map((c) => c.toLowerCase()))
					if (
						childrenLower.has('mods') ||
						childrenLower.has('config') ||
						childrenLower.has('instance.cfg') ||
						childrenLower.has('mmc-pack.json')
					) {
						isInstanceRoot = true
					}
				} catch {}
			}

			const relPrefix = isInstanceRoot ? '' : folderName

			function walk(currentDir, currentRelPrefix) {
				const entries = fs.readdirSync(currentDir, { withFileTypes: true })
				for (const entry of entries) {
					const nameLower = entry.name.toLowerCase()
					if (OS_JUNK_NAMES.has(nameLower)) continue

					const relPath = currentRelPrefix ? `${currentRelPrefix}/${entry.name}` : entry.name
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
						if (policy === 'ignore') continue

						const fileStat = fs.statSync(fullPath)
						pendingMap.set(relPath.toLowerCase(), {
							path: relPath,
							fullPath,
							size: fileStat.size,
							policy,
						})
					}
				}
			}

			walk(resolved, relPrefix)
		}
	}

	const pending = Array.from(pendingMap.values())
	const totalCount = pending.length
	const totalBytes = pending.reduce((sum, item) => sum + (item.size || 0), 0)
	let processedBytes = 0
	let lastEmitTime = 0

	const emitScanProgress = (idx, force = false) => {
		if (typeof emitProgress !== 'function') return
		const now = Date.now()
		if (!force && now - lastEmitTime < 250) return
		lastEmitTime = now
		const ratio = totalBytes > 0 ? processedBytes / totalBytes : (idx + 1) / Math.max(1, totalCount)
		const pct = 5 + Math.min(9, Math.round(ratio * 9))
		emitProgress('pack', pct, `Analizando y calculando huellas de archivos (${idx + 1}/${totalCount})...`)
	}

	if (typeof emitProgress === 'function' && totalCount > 0) {
		emitProgress('pack', 5, `Analizando y calculando huellas de archivos (0/${totalCount})...`)
	}

	const collected = []
	for (let i = 0; i < totalCount; i++) {
		const item = pending[i]
		const sha256 = item.size > 4 * 1024 * 1024 ? await sha256File(item.fullPath) : sha256Hex(fs.readFileSync(item.fullPath))
		processedBytes += item.size
		collected.push({
			path: item.path,
			fullPath: item.fullPath,
			size: item.size,
			sha256,
			policy: item.policy,
		})
		const prevEmit = lastEmitTime
		emitScanProgress(i, i === totalCount - 1)
		if (lastEmitTime !== prevEmit) {
			await new Promise((r) => setImmediate(r))
		}
	}

	collected.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
	return collected
}

function walkSourceDirectory(rootDir, rules) {
	return collectFromSources([rootDir], rules)
}

function createZipWriter(outputZipPath) {
	const fd = fs.openSync(outputZipPath, 'w')
	const zipHasher = crypto.createHash('sha256')
	let offset = 0
	const centralDirectory = []
	let closed = false

	const writeAndHash = (buf) => {
		if (buf.length === 0) return
		fs.writeSync(fd, buf)
		zipHasher.update(buf)
		offset += buf.length
	}

	return {
		writeEntry({ path: relPath, method, crc32: fileCrc, uncompressedSize, payload }) {
			if (centralDirectory.length >= 0xffff) {
				throw new Error('ZIP entry limit exceeded (65,535 entries)')
			}
			const compressedSize = payload.length
			if (
				offset > 0xffffffff ||
				compressedSize > 0xffffffff ||
				uncompressedSize > 0xffffffff
			) {
				throw new Error(`ZIP 32-bit size/offset limit exceeded writing ${relPath}`)
			}
			const nameBuf = Buffer.from(relPath, 'utf8')
			const localHeader = Buffer.alloc(30 + nameBuf.length)
			localHeader.writeUInt32LE(0x04034b50, 0)
			localHeader.writeUInt16LE(20, 4)
			localHeader.writeUInt16LE(0x0800, 6) // UTF-8 flag
			localHeader.writeUInt16LE(method, 8)
			localHeader.writeUInt16LE(0, 10)
			localHeader.writeUInt16LE(0x5421, 12)
			localHeader.writeUInt32LE(fileCrc >>> 0, 14)
			localHeader.writeUInt32LE(compressedSize, 18)
			localHeader.writeUInt32LE(uncompressedSize, 22)
			localHeader.writeUInt16LE(nameBuf.length, 26)
			localHeader.writeUInt16LE(0, 28)
			nameBuf.copy(localHeader, 30)

			const localHeaderOffset = offset
			writeAndHash(localHeader)
			writeAndHash(payload)

			const centralHeader = Buffer.alloc(46 + nameBuf.length)
			centralHeader.writeUInt32LE(0x02014b50, 0)
			centralHeader.writeUInt16LE(20, 4)
			centralHeader.writeUInt16LE(20, 6)
			centralHeader.writeUInt16LE(0x0800, 8)
			centralHeader.writeUInt16LE(method, 10)
			centralHeader.writeUInt16LE(0, 12)
			centralHeader.writeUInt16LE(0x5421, 14)
			centralHeader.writeUInt32LE(fileCrc >>> 0, 16)
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
		},
		finalize() {
			if (closed) {
				throw new Error('ZIP writer already finalized')
			}
			try {
				const cdStart = offset
				if (cdStart > 0xffffffff) {
					throw new Error('ZIP central directory offset exceeds 4GB limit')
				}
				for (const cd of centralDirectory) {
					writeAndHash(cd)
				}
				const cdSize = offset - cdStart
				if (cdSize > 0xffffffff) {
					throw new Error('ZIP central directory size exceeds 4GB limit')
				}
				const eocd = Buffer.alloc(22)
				eocd.writeUInt32LE(0x06054b50, 0)
				eocd.writeUInt16LE(0, 4)
				eocd.writeUInt16LE(0, 6)
				eocd.writeUInt16LE(centralDirectory.length, 8)
				eocd.writeUInt16LE(centralDirectory.length, 10)
				eocd.writeUInt32LE(cdSize, 12)
				eocd.writeUInt32LE(cdStart, 16)
				eocd.writeUInt16LE(0, 20)
				writeAndHash(eocd)
				return {
					size: offset,
					sha256: zipHasher.digest('hex'),
				}
			} finally {
				closed = true
				try {
					fs.closeSync(fd)
				} catch {}
			}
		},
		abort() {
			if (!closed) {
				closed = true
				try {
					fs.closeSync(fd)
				} catch {}
			}
		},
	}
}

let archiver = null
try {
	archiver = require('archiver')
} catch {}

function writeZipWithArchiver(entries, outputZipPath, emitProgress = null) {
	if (!archiver) return null
	return new Promise((resolve, reject) => {
		const output = fs.createWriteStream(outputZipPath, { highWaterMark: 1024 * 1024 })
		const hash = crypto.createHash('sha256')
		let totalBytes = 0
		const totalEntries = entries.length
		const totalInputBytes = entries.reduce((sum, item) => sum + (item.size || 0), 0)
		let lastEmitTime = 0

		const archiveOptions = {
			zlib: { level: 4 },
		}
		const archive =
			typeof archiver === 'function'
				? archiver('zip', archiveOptions)
				: archiver && typeof archiver.ZipArchive === 'function'
					? new archiver.ZipArchive(archiveOptions)
					: null

		if (!archive) {
			try {
				output.close()
			} catch {}
			return resolve(null)
		}

		output.on('close', () => {
			if (typeof emitProgress === 'function') {
				emitProgress('pack', 24, `Paquete ZIP creado (${totalEntries}/${totalEntries} archivos).`)
			}
			resolve({
				size: totalBytes,
				sha256: hash.digest('hex'),
			})
		})

		archive.on('data', (chunk) => {
			hash.update(chunk)
			totalBytes += chunk.length
		})

		archive.on('progress', (prog) => {
			if (typeof emitProgress !== 'function') return
			const now = Date.now()
			if (now - lastEmitTime < 250) return
			lastEmitTime = now
			const processedBytes = prog?.fs?.processedBytes || 0
			const processedEntries = prog?.entries?.processed || 0
			const ratio =
				totalInputBytes > 0
					? Math.min(1, processedBytes / totalInputBytes)
					: Math.min(1, processedEntries / Math.max(1, totalEntries))
			const pct = 15 + Math.round(ratio * 9)
			emitProgress('pack', pct, `Empaquetando archivos (${processedEntries}/${totalEntries})...`)
		})

		archive.on('error', (err) => {
			reject(err)
		})

		archive.pipe(output)

		for (const item of entries) {
			const ext = path.posix.extname(item.path).toLowerCase()
			const isStored = item.size === 0 || PRECOMPRESSED_EXTS.has(ext)
			archive.file(item.fullPath, {
				name: item.path,
				store: isStored,
			})
		}

		archive.finalize()
	})
}

function writeZipDeterministic(entries, outputZipPath) {
	const writer = createZipWriter(outputZipPath)
	try {
		for (const item of entries) {
			const rawData = fs.readFileSync(item.fullPath)
			const uncompressedSize = rawData.length
			const fileCrc = crc32(rawData)
			const ext = path.posix.extname(item.path).toLowerCase()
			if (uncompressedSize === 0 || PRECOMPRESSED_EXTS.has(ext)) {
				writer.writeEntry({
					path: item.path,
					method: 0,
					crc32: fileCrc,
					uncompressedSize,
					payload: rawData,
				})
				continue
			}

			const deflated = zlib.deflateRawSync(rawData, { level: 1 })
			const useDeflate = deflated.length < uncompressedSize
			writer.writeEntry({
				path: item.path,
				method: useDeflate ? 8 : 0,
				crc32: fileCrc,
				uncompressedSize,
				payload: useDeflate ? deflated : rawData,
			})
		}
		return writer.finalize()
	} catch (err) {
		writer.abort()
		throw err
	}
}

function readFully(fd, buffer, offset, length, position) {
	let totalRead = 0
	while (totalRead < length) {
		const bytesRead = fs.readSync(fd, buffer, offset + totalRead, length - totalRead, position + totalRead)
		if (bytesRead <= 0) {
			throw new Error('Unexpected end of file while reading ZIP archive')
		}
		totalRead += bytesRead
	}
}

function parseZip64Extra(extraBuf, needUncomp, needComp, needLocalOff) {
	let ptr = 0
	let uncomp = null
	let comp = null
	let localOff = null
	while (ptr + 4 <= extraBuf.length) {
		const headerId = extraBuf.readUInt16LE(ptr)
		const dataSize = extraBuf.readUInt16LE(ptr + 2)
		ptr += 4
		if (headerId === 0x0001) {
			let cur = ptr
			if (needUncomp && cur + 8 <= ptr + dataSize) {
				uncomp = Number(extraBuf.readBigUInt64LE(cur))
				cur += 8
			}
			if (needComp && cur + 8 <= ptr + dataSize) {
				comp = Number(extraBuf.readBigUInt64LE(cur))
				cur += 8
			}
			if (needLocalOff && cur + 8 <= ptr + dataSize) {
				localOff = Number(extraBuf.readBigUInt64LE(cur))
			}
			break
		}
		ptr += dataSize
	}
	return { uncomp, comp, localOff }
}

const STANDARD_MODPACK_TOP_DIRS = new Set([
	'mods',
	'config',
	'defaultconfigs',
	'resourcepacks',
	'shaderpacks',
	'datapacks',
	'scripts',
	'kubejs',
	'saves',
])

function detectWrapperPrefix(rawNames) {
	let commonPrefix = null
	for (const name of rawNames) {
		const slashIdx = name.indexOf('/')
		if (slashIdx === -1) {
			return null
		}
		const candidate = name.slice(0, slashIdx + 1)
		if (commonPrefix === null) {
			commonPrefix = candidate
		} else if (commonPrefix !== candidate) {
			return null
		}
	}
	if (!commonPrefix) return null
	const topFolder = commonPrefix.slice(0, -1).toLowerCase()
	return STANDARD_MODPACK_TOP_DIRS.has(topFolder) ? null : commonPrefix
}

function readZipDirectoryRecords(fd, fileSize, zipPath) {
	if (fileSize < 22) {
		throw new Error(`El archivo ZIP está vacío o dañado: ${zipPath}`)
	}

	const magicBuf = Buffer.allocUnsafe(4)
	readFully(fd, magicBuf, 0, 4, 0)
	const firstSig = magicBuf.readUInt32LE(0)
	if (firstSig !== 0x04034b50 && firstSig !== 0x06054b50) {
		throw new Error('El archivo seleccionado no es un ZIP válido (posiblemente corrupto o renombrado).')
	}

	const tailLen = Math.min(fileSize, 65557)
	const tailStart = fileSize - tailLen
	const tailBuf = Buffer.allocUnsafe(tailLen)
	readFully(fd, tailBuf, 0, tailLen, tailStart)

	let eocdRel = -1
	let fallbackRel = -1
	for (let i = tailLen - 22; i >= 0; i--) {
		if (tailBuf.readUInt32LE(i) === 0x06054b50) {
			if (fallbackRel === -1) fallbackRel = i
			const commentLen = tailBuf.readUInt16LE(i + 20)
			if (i + 22 + commentLen === tailLen) {
				eocdRel = i
				break
			}
		}
	}
	if (eocdRel === -1) eocdRel = fallbackRel
	if (eocdRel === -1) {
		throw new Error(`Invalid ZIP archive (EOCD not found): ${zipPath}`)
	}

	let totalEntries = tailBuf.readUInt16LE(eocdRel + 10)
	let cdSize = tailBuf.readUInt32LE(eocdRel + 12)
	let cdOffset = tailBuf.readUInt32LE(eocdRel + 16)

	if (totalEntries === 0xffff || cdSize === 0xffffffff || cdOffset === 0xffffffff) {
		const eocdAbs = tailStart + eocdRel
		if (eocdAbs < 20) {
			throw new Error(`Invalid ZIP64 locator offset in archive: ${zipPath}`)
		}
		const locBuf = Buffer.allocUnsafe(20)
		readFully(fd, locBuf, 0, 20, eocdAbs - 20)
		if (locBuf.readUInt32LE(0) !== 0x07064b50) {
			throw new Error(`ZIP64 archive missing locator record: ${zipPath}`)
		}
		const zip64EocdOffset = Number(locBuf.readBigUInt64LE(8))
		const z64Buf = Buffer.allocUnsafe(56)
		readFully(fd, z64Buf, 0, 56, zip64EocdOffset)
		if (z64Buf.readUInt32LE(0) !== 0x06064b50) {
			throw new Error(`Invalid ZIP64 EOCD signature: ${zipPath}`)
		}
		totalEntries = Number(z64Buf.readBigUInt64LE(32))
		cdSize = Number(z64Buf.readBigUInt64LE(40))
		cdOffset = Number(z64Buf.readBigUInt64LE(48))
	}

	if (cdOffset + cdSize > fileSize) {
		throw new Error(`Invalid Central Directory bounds in ZIP archive: ${zipPath}`)
	}

	const cdBuf = Buffer.allocUnsafe(cdSize)
	readFully(fd, cdBuf, 0, cdSize, cdOffset)

	const records = []
	let ptr = 0
	for (let i = 0; i < totalEntries; i++) {
		if (ptr + 46 > cdBuf.length || cdBuf.readUInt32LE(ptr) !== 0x02014b50) {
			throw new Error(`Invalid Central Directory header at offset ${ptr}`)
		}
		const flags = cdBuf.readUInt16LE(ptr + 8)
		if (flags & 0x0001) {
			throw new Error('El archivo ZIP contiene entradas cifradas con contraseña; descífralo antes de subirlo.')
		}
		const method = cdBuf.readUInt16LE(ptr + 10)
		const entryCrc = cdBuf.readUInt32LE(ptr + 16)
		let compressedSize = cdBuf.readUInt32LE(ptr + 20)
		let uncompressedSize = cdBuf.readUInt32LE(ptr + 24)
		const nameLen = cdBuf.readUInt16LE(ptr + 28)
		const extraLen = cdBuf.readUInt16LE(ptr + 30)
		const commentLen = cdBuf.readUInt16LE(ptr + 32)
		let localHeaderOffset = cdBuf.readUInt32LE(ptr + 42)
		const rawName = cdBuf.subarray(ptr + 46, ptr + 46 + nameLen).toString('utf8')

		if (
			compressedSize === 0xffffffff ||
			uncompressedSize === 0xffffffff ||
			localHeaderOffset === 0xffffffff
		) {
			const extraBuf = cdBuf.subarray(ptr + 46 + nameLen, ptr + 46 + nameLen + extraLen)
			const z64 = parseZip64Extra(
				extraBuf,
				uncompressedSize === 0xffffffff,
				compressedSize === 0xffffffff,
				localHeaderOffset === 0xffffffff,
			)
			if (z64.uncomp !== null) uncompressedSize = z64.uncomp
			if (z64.comp !== null) compressedSize = z64.comp
			if (z64.localOff !== null) localHeaderOffset = z64.localOff
		}

		ptr += 46 + nameLen + extraLen + commentLen
		records.push({
			rawName,
			method,
			flags,
			crc32: entryCrc,
			compressedSize,
			uncompressedSize,
			localHeaderOffset,
		})
	}
	return records
}

function readZipEntrySlice(fd, fileSize, rec, localHdrBuf) {
	readFully(fd, localHdrBuf, 0, 30, rec.localHeaderOffset)
	if (localHdrBuf.readUInt32LE(0) !== 0x04034b50) {
		throw new Error(`Invalid Local File Header for ZIP entry ${rec.rawName}`)
	}
	const localNameLen = localHdrBuf.readUInt16LE(26)
	const localExtraLen = localHdrBuf.readUInt16LE(28)
	const dataStart = rec.localHeaderOffset + 30 + localNameLen + localExtraLen
	if (dataStart + rec.compressedSize > fileSize) {
		throw new Error(`Truncated data for ZIP entry ${rec.rawName}`)
	}

	const compSlice = Buffer.allocUnsafe(rec.compressedSize)
	if (rec.compressedSize > 0) {
		readFully(fd, compSlice, 0, rec.compressedSize, dataStart)
	}
	return compSlice
}

function decompressAndVerifyEntry(rec, compSlice) {
	let rawData
	if (rec.method === 0) {
		rawData = compSlice
	} else if (rec.method === 8) {
		rawData = zlib.inflateRawSync(compSlice)
	} else {
		throw new Error(`Unsupported ZIP compression method ${rec.method} for entry ${rec.rawName}`)
	}

	if (rawData.length !== rec.uncompressedSize) {
		throw new Error(`Uncompressed size mismatch for ${rec.rawName}`)
	}
	const actualCrc = crc32(rawData)
	if (rec.crc32 && actualCrc !== rec.crc32) {
		throw new Error(`CRC-32 mismatch for ${rec.rawName}`)
	}
	return { rawData, crc32: actualCrc }
}

function readZipEntriesMap(zipPath, options = {}) {
	const includeData = options.includeData !== false
	const fd = fs.openSync(zipPath, 'r')
	try {
		const fileSize = fs.fstatSync(fd).size
		const records = readZipDirectoryRecords(fd, fileSize, zipPath)
		const localHdrBuf = Buffer.allocUnsafe(30)
		const entries = new Map()

		for (const rec of records) {
			if (rec.rawName.endsWith('/') || rec.rawName.endsWith('\\')) continue
			const compSlice = readZipEntrySlice(fd, fileSize, rec, localHdrBuf)
			const { rawData } = decompressAndVerifyEntry(rec, compSlice)
			const entryObj = {
				size: rawData.length,
				sha256: sha256Hex(rawData),
			}
			if (includeData) {
				entryObj.data = rawData
			}
			entries.set(rec.rawName, entryObj)
		}

		return entries
	} finally {
		fs.closeSync(fd)
	}
}

async function prepareInputZip(srcZipPath, outputZipPath, rules, emitProgress) {
	const fd = fs.openSync(srcZipPath, 'r')
	let writer = null
	try {
		const fileSize = fs.fstatSync(fd).size
		const allRecords = readZipDirectoryRecords(fd, fileSize, srcZipPath)
		const fileRecords = allRecords.filter(
			(r) => r.rawName && !r.rawName.endsWith('/') && !r.rawName.endsWith('\\'),
		)
		const normalizedRawNames = fileRecords.map((r) =>
			r.rawName.replace(/\\/g, '/').replace(/^\/+/, ''),
		)
		const commonPrefix = detectWrapperPrefix(normalizedRawNames)
		const hasWrapperFolder = Boolean(commonPrefix)

		const planned = []
		const seenLower = new Set()
		let needsRepack = false

		for (let i = 0; i < fileRecords.length; i++) {
			const rec = fileRecords[i]
			let rel = normalizedRawNames[i]
			if (!rel) {
				needsRepack = true
				continue
			}
			if (rec.rawName !== rel) {
				needsRepack = true
			}
			if (commonPrefix && rel.startsWith(commonPrefix)) {
				rel = rel.slice(commonPrefix.length).replace(/^\/+/, '')
				needsRepack = true
			}
			if (!rel) {
				needsRepack = true
				continue
			}
			if (OS_JUNK_NAMES.has(path.posix.basename(rel).toLowerCase())) {
				needsRepack = true
				continue
			}
			validateSafeRelativePath(rel)
			const lowerKey = rel.toLowerCase()
			if (seenLower.has(lowerKey)) {
				throw new Error(`Case-insensitive duplicate path detected: ${rel}`)
			}
			seenLower.add(lowerKey)
			const policy = resolvePolicy(rules, rel)
			if (policy === 'ignore') {
				needsRepack = true
				continue
			}
			planned.push({ rec, rel, policy })
		}

		if (planned.length === 0) {
			throw new Error('El archivo ZIP no contiene archivos válidos después de aplicar las reglas de exclusión.')
		}

		const usedDirectZip =
			srcZipPath.toLowerCase().endsWith('.zip') && !hasWrapperFolder && !needsRepack

		const localHdrBuf = Buffer.allocUnsafe(30)
		const totalItems = planned.length
		const collectedFiles = []
		const entryCrcs = new Map()
		let lastEmitTime = 0

		if (usedDirectZip) {
			emitProgress('pack', 10, 'Verificando integridad y huellas SHA-256 del ZIP...')
			for (let i = 0; i < totalItems; i++) {
				const { rec, rel, policy } = planned[i]
				const compSlice = readZipEntrySlice(fd, fileSize, rec, localHdrBuf)
				const { rawData } = decompressAndVerifyEntry(rec, compSlice)
				collectedFiles.push({
					path: rel,
					sha256: sha256Hex(rawData),
					size: rawData.length,
					policy,
				})
				const now = Date.now()
				if (now - lastEmitTime >= 250) {
					lastEmitTime = now
					const pct = 10 + Math.round(((i + 1) / totalItems) * 8)
					emitProgress('pack', pct, `Verificando archivos del ZIP (${i + 1}/${totalItems})...`)
				}
			}
			collectedFiles.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
		} else {
			emitProgress('pack', 12, 'Limpiando estructura y reempaquetando .zip sin recomprimir...')
			planned.sort((a, b) => (a.rel < b.rel ? -1 : a.rel > b.rel ? 1 : 0))
			writer = createZipWriter(outputZipPath)
			for (let i = 0; i < totalItems; i++) {
				const { rec, rel, policy } = planned[i]
				const compSlice = readZipEntrySlice(fd, fileSize, rec, localHdrBuf)
				const { rawData, crc32: actualCrc } = decompressAndVerifyEntry(rec, compSlice)
				const entrySha = sha256Hex(rawData)
				writer.writeEntry({
					path: rel,
					method: rec.method,
					crc32: actualCrc,
					uncompressedSize: rec.uncompressedSize,
					payload: compSlice,
				})
				entryCrcs.set(rel, actualCrc)
				collectedFiles.push({
					path: rel,
					sha256: entrySha,
					size: rec.uncompressedSize,
					policy,
				})
				const now = Date.now()
				if (now - lastEmitTime >= 250) {
					lastEmitTime = now
					const pct = 12 + Math.round(((i + 1) / totalItems) * 10)
					emitProgress('pack', pct, `Empaquetando archivos (${i + 1}/${totalItems})...`)
				}
			}
		}

		if (usedDirectZip) {
			fs.closeSync(fd)
			emitProgress('pack', 19, 'Calculando SHA-256 final del ZIP...')
			const zipSha256 = await sha256File(srcZipPath)
			return {
				usedDirectZip: true,
				outputZipPath: srcZipPath,
				zipSize: fileSize,
				zipSha256,
				collectedFiles,
				entryCrcs,
			}
		}

		const finalized = writer.finalize()
		writer = null
		return {
			usedDirectZip: false,
			outputZipPath,
			zipSize: finalized.size,
			zipSha256: finalized.sha256,
			collectedFiles,
			entryCrcs,
		}
	} finally {
		if (writer) {
			writer.abort()
		}
		try {
			fs.closeSync(fd)
		} catch {}
	}
}

function verifyZipEntryHeaderBounds(fd, fileSize, rec, localHdrBuf) {
	readFully(fd, localHdrBuf, 0, 30, rec.localHeaderOffset)
	if (localHdrBuf.readUInt32LE(0) !== 0x04034b50) {
		throw new Error(`Invalid Local File Header for ZIP entry ${rec.rawName}`)
	}
	const localNameLen = localHdrBuf.readUInt16LE(26)
	const localExtraLen = localHdrBuf.readUInt16LE(28)
	const dataStart = rec.localHeaderOffset + 30 + localNameLen + localExtraLen
	if (dataStart + rec.compressedSize > fileSize) {
		throw new Error(`Truncated data for ZIP entry ${rec.rawName}`)
	}
}

function selfCheckZipHeadersAgainstManifest(zipPath, manifest, expectedCrcs) {
	const fd = fs.openSync(zipPath, 'r')
	try {
		const fileSize = fs.fstatSync(fd).size
		const records = readZipDirectoryRecords(fd, fileSize, zipPath)
		const byName = new Map()
		const localHdrBuf = Buffer.allocUnsafe(30)
		for (const rec of records) {
			if (rec.rawName.endsWith('/') || rec.rawName.endsWith('\\')) continue
			verifyZipEntryHeaderBounds(fd, fileSize, rec, localHdrBuf)
			byName.set(rec.rawName, rec)
		}
		for (const fileEntry of manifest.files) {
			const rec = byName.get(fileEntry.path)
			if (!rec) {
				throw new Error(`Self-check failed: manifest entry missing from ZIP: ${fileEntry.path}`)
			}
			if (rec.uncompressedSize !== fileEntry.size) {
				throw new Error(
					`Self-check failed: size mismatch for ${fileEntry.path} (manifest=${fileEntry.size}, zip=${rec.uncompressedSize})`,
				)
			}
			const expCrc = expectedCrcs.get(fileEntry.path)
			if (expCrc !== undefined && rec.crc32 !== expCrc) {
				throw new Error(
					`Self-check failed: CRC mismatch for ${fileEntry.path} (expected=${expCrc}, zip=${rec.crc32})`,
				)
			}
		}
	} finally {
		fs.closeSync(fd)
	}
}

function selfCheckZipAgainstManifest(zipPath, manifest) {
	const zipEntries = readZipEntriesMap(zipPath, { includeData: false })
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

const UPLOAD_HIGH_WATER_MARK = 8 * 1024 * 1024
const FILE_READ_CHUNK_SIZE = 512 * 1024
const UPLOAD_TIMEOUT_MS = 300000
const UPLOAD_HTTPS_AGENT = new https.Agent({ keepAlive: true, maxSockets: 4 })

function githubApiRequest(endpoint, token, options = {}) {
	return new Promise((resolve, reject) => {
		const url = endpoint.startsWith('http')
			? new URL(endpoint)
			: new URL(`https://api.github.com${endpoint}`)
		const headers = {
			'User-Agent': 'Dr3shLauncher/1.1.0 (+https://github.com/Dr3sh/dr3shapp)',
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

		const timeoutMs = options.timeoutMs || 120000
		let progressTimer = null
		const cleanupTimers = () => {
			if (progressTimer) {
				clearInterval(progressTimer)
				progressTimer = null
			}
		}
		const cleanupBody = () => {
			cleanupTimers()
			if (options.body && typeof options.body.destroy === 'function') {
				try {
					options.body.destroy()
				} catch {}
			}
		}

		let reqSocket = null
		let bodyStartSocketBytes = null
		let streamEnded = false
		let settled = false
		let onDrainCallback = null
		let lastActivityAt = Date.now()

		const req = https.request(
			{
				hostname: url.hostname,
				path: url.pathname + url.search,
				method: options.method || 'GET',
				headers,
				...(options.highWaterMark ? { highWaterMark: options.highWaterMark } : {}),
				...(options.agent !== undefined ? { agent: options.agent } : {}),
			},
			(res) => {
				cleanupTimers()
				const chunks = []
				res.on('data', (chunk) => {
					lastActivityAt = Date.now()
					chunks.push(chunk)
				})
				res.on('error', () => {
					cleanupTimers()
				})
				res.on('close', () => {
					cleanupTimers()
				})
				res.on('end', () => {
					settled = true
					const text = Buffer.concat(chunks).toString('utf8')
					try {
						const json = JSON.parse(text)
						if (res.statusCode >= 200 && res.statusCode < 300) {
							resolve(json)
						} else {
							const err = new Error(`GitHub API Error ${res.statusCode}: ${json.message || text}`)
							err.statusCode = res.statusCode
							reject(err)
						}
					} catch {
						if (res.statusCode >= 200 && res.statusCode < 300) {
							resolve(text)
						} else {
							const err = new Error(`GitHub HTTP ${res.statusCode}: ${text}`)
							err.statusCode = res.statusCode
							reject(err)
						}
					}
				})
			},
		)

		req.setTimeout(timeoutMs, () => {
			settled = true
			cleanupBody()
			req.destroy(
				new Error(
					`Tiempo de espera agotado conectando con GitHub (${Math.round(timeoutMs / 1000)}s sin respuesta).`,
				),
			)
		})
		req.on('drain', () => {
			if (onDrainCallback) onDrainCallback()
		})
		req.on('socket', (sock) => {
			reqSocket = sock
			if (typeof sock.setNoDelay === 'function') {
				sock.setNoDelay(true)
			}
			sock.on('drain', () => {
				if (onDrainCallback) onDrainCallback()
			})
			if (!sock.connecting) {
				if (bodyStartSocketBytes === null) {
					bodyStartSocketBytes = sock.bytesWritten || 0
				}
				if (onDrainCallback) onDrainCallback()
			} else {
				sock.once('secureConnect', () => {
					if (bodyStartSocketBytes === null) {
						bodyStartSocketBytes = sock.bytesWritten || 0
					}
					if (onDrainCallback) onDrainCallback()
				})
			}
		})
		req.on('error', (err) => {
			settled = true
			cleanupBody()
			reject(err)
		})
		if (options.body) {
			if (typeof options.body.pipe === 'function') {
				const pauseThreshold = 8 * 1024 * 1024
				const resumeThreshold = 4 * 1024 * 1024
				let bodyPaused = false

				const getQueuedBytes = () =>
					(reqSocket ? (reqSocket.writableLength || 0) : 0) + (req.writableLength || 0)

				const maybeResumeBody = () => {
					if (settled || streamEnded || !bodyPaused) return
					if (getQueuedBytes() < resumeThreshold) {
						bodyPaused = false
						options.body.resume()
					}
				}
				onDrainCallback = maybeResumeBody

				options.body.on('data', (chunk) => {
					if (settled) return
					if (bodyStartSocketBytes === null && reqSocket && !reqSocket.connecting) {
						bodyStartSocketBytes = reqSocket.bytesWritten || 0
					}
					req.write(chunk)
					if (!bodyPaused && getQueuedBytes() > pauseThreshold) {
						bodyPaused = true
						options.body.pause()
					}
				})
				options.body.on('end', () => {
					streamEnded = true
					if (!settled) {
						req.end()
					}
				})
				options.body.on('error', (err) => {
					settled = true
					cleanupTimers()
					req.destroy(err)
				})

				const totalBytes = options.totalBytes || 0
				const hasProgressCb = typeof options.onUploadProgress === 'function' && totalBytes > 0
				const startTime = Date.now()
				let lastEmitBytes = -1
				let lastEmitWaiting = false
				let lastProgressTick = startTime
				let lastTimeoutRefresh = startTime
				let lastObservedSocketBytes = 0
				let lastMonotonicBytes = 0
				let emaSpeedBps = 0
				const speedSamples = [{ time: startTime, bytes: 0 }]

				if (hasProgressCb) {
					options.onUploadProgress({
						uploadedBytes: 0,
						totalBytes,
						speedBps: 0,
						etaSeconds: null,
						waitingAck: false,
					})
				}

				progressTimer = setInterval(() => {
					if (settled) {
						cleanupTimers()
						return
					}

					maybeResumeBody()

					const now = Date.now()
					const curSocketBytes = reqSocket ? (reqSocket.bytesWritten || 0) : 0
					if (curSocketBytes > lastObservedSocketBytes) {
						lastObservedSocketBytes = curSocketBytes
						lastActivityAt = now
						if (
							now - lastTimeoutRefresh >= 1000 &&
							reqSocket &&
							typeof reqSocket.setTimeout === 'function'
						) {
							lastTimeoutRefresh = now
							reqSocket.setTimeout(timeoutMs)
						}
					} else if (now - lastActivityAt >= timeoutMs) {
						settled = true
						cleanupBody()
						req.destroy(
							new Error(
								`Tiempo de espera agotado conectando con GitHub (${Math.round(timeoutMs / 1000)}s sin actividad).`,
							),
						)
						return
					}

					if (!hasProgressCb) return

					let rawSent = 0
					let socketWritableLen = 0
					const totalQueuedLen = getQueuedBytes()
					if (reqSocket && bodyStartSocketBytes !== null) {
						socketWritableLen = reqSocket.writableLength || 0
						rawSent = Math.max(0, curSocketBytes - bodyStartSocketBytes - socketWritableLen)
					}
					const waitingAck = Boolean(streamEnded && totalQueuedLen === 0)
					const sentBytes = waitingAck
						? totalBytes
						: Math.max(lastMonotonicBytes, Math.min(Math.max(0, totalBytes - 1), rawSent))
					lastMonotonicBytes = sentBytes

					const dueProgressTick = now - lastProgressTick >= 500
					if (!dueProgressTick && waitingAck === lastEmitWaiting) {
						return
					}
					if (dueProgressTick) {
						lastProgressTick = now
						speedSamples.push({ time: now, bytes: sentBytes })
						while (speedSamples.length > 2 && now - speedSamples[0].time > 12000) {
							speedSamples.shift()
						}
						const oldest = speedSamples[0]
						const winDt = (now - oldest.time) / 1000
						if (winDt >= 0.4 && sentBytes >= oldest.bytes) {
							const windowSpeed = (sentBytes - oldest.bytes) / winDt
							emaSpeedBps = emaSpeedBps > 0 ? emaSpeedBps * 0.82 + windowSpeed * 0.18 : windowSpeed
						} else if (emaSpeedBps === 0 && sentBytes > 0) {
							const elapsed = Math.max(0.5, (now - startTime) / 1000)
							emaSpeedBps = sentBytes / elapsed
						}
					}

					const etaSeconds =
						!waitingAck && emaSpeedBps > 1024
							? Math.max(1, Math.round((totalBytes - sentBytes) / emaSpeedBps))
							: null

					if (sentBytes !== lastEmitBytes || waitingAck !== lastEmitWaiting || dueProgressTick) {
						lastEmitBytes = sentBytes
						lastEmitWaiting = waitingAck
						options.onUploadProgress({
							uploadedBytes: sentBytes,
							totalBytes,
							speedBps: Math.round(emaSpeedBps),
							etaSeconds,
							waitingAck,
						})
					}
				}, 20)
			} else {
				req.write(options.body)
				req.end()
			}
		} else {
			req.end()
		}
	})
}

async function uploadReleaseAsset(uploadUrlTemplate, token, filePath, fileName, onByteProgress) {
	const uploadUrl = uploadUrlTemplate.replace(
		/\{(\?.*)?\}$/,
		`?name=${encodeURIComponent(fileName)}`,
	)
	const fileSize = fs.statSync(filePath).size
	const fileStream = fs.createReadStream(filePath, { highWaterMark: FILE_READ_CHUNK_SIZE })
	return await githubApiRequest(uploadUrl, token, {
		method: 'POST',
		highWaterMark: UPLOAD_HIGH_WATER_MARK,
		agent: UPLOAD_HTTPS_AGENT,
		timeoutMs: UPLOAD_TIMEOUT_MS,
		totalBytes: fileSize,
		onUploadProgress: typeof onByteProgress === 'function' ? onByteProgress : undefined,
		headers: {
			'Content-Type': fileName.endsWith('.zip') ? 'application/zip' : 'application/json',
			'Content-Length': fileSize,
		},
		body: fileStream,
	})
}

async function downloadJsonWithRedirects(url, maxRedirects = 5) {
	return new Promise((resolve, reject) => {
		const visit = (currentUrl, remaining) => {
			let parsed
			try {
				parsed = new URL(currentUrl)
			} catch (err) {
				reject(new Error(`URL inválida para descargar JSON: ${currentUrl}`))
				return
			}
			const client = parsed.protocol === 'http:' ? http : https
			const req = client.get(
				{
					hostname: parsed.hostname,
					port: parsed.port || undefined,
					path: parsed.pathname + parsed.search,
					headers: {
						'User-Agent': 'Dr3sh-Launcher-Publisher/1.0',
						Accept: 'application/json, text/plain, */*',
					},
				},
				(res) => {
					const status = res.statusCode || 0
					if ([301, 302, 303, 307, 308].includes(status) && res.headers.location) {
						res.resume()
						if (remaining <= 0) {
							reject(new Error(`Demasiadas redirecciones al descargar ${url}`))
							return
						}
						const nextUrl = new URL(res.headers.location, currentUrl).toString()
						visit(nextUrl, remaining - 1)
						return
					}
					const chunks = []
					res.on('data', (chunk) => chunks.push(chunk))
					res.on('end', () => {
						const body = Buffer.concat(chunks).toString('utf8')
						if (status < 200 || status >= 300) {
							const err = new Error(`HTTP ${status} al descargar ${currentUrl}`)
							err.statusCode = status
							reject(err)
							return
						}
						try {
							resolve(JSON.parse(body))
						} catch (parseErr) {
							reject(new Error(`JSON inválido recibido desde ${currentUrl}: ${parseErr.message}`))
						}
					})
				},
			)
			req.setTimeout(30000, () => {
				req.destroy(new Error(`Tiempo de espera agotado descargando ${currentUrl}`))
			})
			req.on('error', reject)
		}
		visit(url, maxRedirects)
	})
}

async function publishModpackViaPatchAction({
	cleanPackId,
	cleanVersion,
	tagName,
	zipFileName,
	zipDownloadUrl,
	manifestDownloadUrl,
	repo,
	minecraftVersion,
	loader,
	loaderVersion,
	title,
	description,
	versionsFile,
	explicitToken,
	workDir,
	collectedFiles,
	baseManifestUrl,
	baseTag,
	emitProgress,
}) {
	emitProgress('pack', 10, 'Descargando manifest.json de la versión anterior...')
	let baseManifest
	try {
		baseManifest = await downloadJsonWithRedirects(baseManifestUrl)
	} catch (err) {
		console.warn(`[fast-patch] No se pudo descargar manifest base (${baseManifestUrl}): ${err.message}`)
		return null
	}
	if (!baseManifest || !Array.isArray(baseManifest.files)) {
		console.warn('[fast-patch] El manifest base no tiene lista de archivos válida; usando subida completa.')
		return null
	}

	const baseMap = new Map()
	for (const entry of baseManifest.files) {
		if (entry && entry.path) {
			baseMap.set(String(entry.path), {
				sha256: String(entry.sha256 || '').toLowerCase(),
				size: Number(entry.size || 0),
			})
		}
	}

	const currentMap = new Map()
	const changedOrAdded = []
	for (const file of collectedFiles) {
		currentMap.set(file.path, file)
		const prev = baseMap.get(file.path)
		if (
			!prev ||
			prev.sha256 !== String(file.sha256 || '').toLowerCase() ||
			prev.size !== Number(file.size || 0)
		) {
			changedOrAdded.push(file)
		}
	}

	const removedPaths = []
	for (const prevPath of baseMap.keys()) {
		if (!currentMap.has(prevPath)) {
			removedPaths.push(prevPath)
		}
	}

	const changedBytes = changedOrAdded.reduce((sum, f) => sum + (Number(f.size) || 0), 0)
	const baseZipSize =
		Number(baseManifest?.zip?.size) ||
		baseManifest.files.reduce((sum, f) => sum + (Number(f.size) || 0), 0)

	if (baseZipSize > 0 && changedBytes >= baseZipSize * 0.75) {
		emitProgress(
			'pack',
			12,
			'Más del 75% del modpack cambió respecto a la versión anterior; usando subida completa...',
		)
		return null
	}

	const token = resolveGithubToken(explicitToken)
	if (!token) {
		throw new Error(
			'GitHub token not found. Set GITHUB_TOKEN env var or place token in active_token.txt.',
		)
	}

	const changedMb = (changedBytes / (1024 * 1024)).toFixed(1)
	emitProgress(
		'pack',
		14,
		`Parche detectado: ${changedOrAdded.length} archivo(s) (${changedMb} MB) y ${removedPaths.length} eliminado(s). Empaquetando parche...`,
	)

	const patchZipPath = path.join(workDir, 'patch.zip')
	const removedJsonPath = path.join(workDir, 'removed.json')
	writeZipDeterministic(changedOrAdded, patchZipPath)
	fs.writeFileSync(removedJsonPath, `${JSON.stringify(removedPaths, null, 2)}\n`, 'utf8')

	const stagingTag = `patch-staging-${tagName}-${Date.now()}`
	let stagingRelease = null
	let dispatchedToActions = false

	const cleanupStaging = async () => {
		if (!stagingRelease) return
		try {
			await githubApiRequest(`/repos/${repo}/releases/${stagingRelease.id}`, token, {
				method: 'DELETE',
			})
		} catch {}
		try {
			await githubApiRequest(
				`/repos/${repo}/git/refs/tags/${encodeURIComponent(stagingTag)}`,
				token,
				{ method: 'DELETE' },
			)
		} catch {}
	}

	try {
		emitProgress('release', 18, `Creando release temporal de parche (${stagingTag})...`)
		stagingRelease = await githubApiRequest(`/repos/${repo}/releases`, token, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({
				tag_name: stagingTag,
				name: `Staging ${tagName}`,
				body: 'Temporary patch staging release (auto-deleted by GitHub Actions)',
				draft: false,
				prerelease: true,
			}),
		})

		await uploadReleaseAsset(stagingRelease.upload_url, token, patchZipPath, 'patch.zip', (progress) => {
			const { uploadedBytes, totalBytes, speedBps, etaSeconds, waitingAck } = progress
			const ratio = totalBytes > 0 ? uploadedBytes / totalBytes : 0
			const percent = waitingAck ? 62 : Math.min(61, 20 + Math.round(ratio * 41))
			const msg = waitingAck
				? 'Subiendo patch.zip (esperando confirmación de GitHub...)'
				: `Subiendo parche ligero (${changedOrAdded.length} archivos)...`
			emitProgress('upload_zip', percent, msg, {
				uploadedBytes,
				totalBytes,
				speedBps,
				etaSeconds,
			})
		})

		emitProgress('upload_zip', 64, 'Subiendo lista de archivos eliminados (removed.json)...')
		await uploadReleaseAsset(stagingRelease.upload_url, token, removedJsonPath, 'removed.json')

		const jobId = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`
		emitProgress('release', 66, 'Iniciando ensamblado rápido en GitHub Actions...')

		const baseRepoMatch = String(baseManifestUrl).match(
			/github\.com\/([^/]+\/[^/]+)\/releases\/download\//i,
		)
		const baseRepo = baseRepoMatch ? baseRepoMatch[1] : repo

		try {
			await githubApiRequest(
				`/repos/${repo}/actions/workflows/build-patch-release.yml/dispatches`,
				token,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						ref: 'main',
						inputs: {
							pack_id: cleanPackId,
							version: cleanVersion,
							title: title || `${cleanPackId} v${cleanVersion}`,
							description: description || '',
							minecraft_version: minecraftVersion || '1.21.1',
							loader: loader || 'fabric',
							loader_version: loaderVersion || '',
							base_repo: baseRepo,
							base_tag: baseTag,
							patch_tag: stagingTag,
							job_id: jobId,
						},
					}),
				},
			)
			dispatchedToActions = true
		} catch (dispatchErr) {
			if (dispatchErr && dispatchErr.statusCode === 404) {
				await cleanupStaging()
				emitProgress(
					'pack',
					12,
					'Workflow build-patch-release.yml no encontrado en el repo; usando subida completa...',
				)
				return null
			}
			throw dispatchErr
		}

		const startedAt = Date.now()
		const maxWaitMs = 20 * 60 * 1000
		let matchedRun = null
		let pollCount = 0

		while (Date.now() - startedAt < maxWaitMs) {
			await new Promise((r) => setTimeout(r, 3000))
			pollCount++

			const runsData = await githubApiRequest(
				`/repos/${repo}/actions/runs?event=workflow_dispatch&per_page=15`,
				token,
			)
			const runs = Array.isArray(runsData?.workflow_runs) ? runsData.workflow_runs : []
			matchedRun = runs.find(
				(r) =>
					r &&
					((typeof r.name === 'string' && r.name.includes(`[${jobId}]`)) ||
						(typeof r.display_title === 'string' && r.display_title.includes(`[${jobId}]`))),
			)

			if (!matchedRun) {
				emitProgress('release', 68, 'Esperando a que GitHub Actions inicie el ensamblado...')
				continue
			}

			if (matchedRun.status !== 'completed') {
				const simulatedPercent = Math.min(91, 70 + Math.floor(pollCount * 1.2))
				const phaseMsg =
					matchedRun.status === 'queued'
						? 'En cola de GitHub Actions esperando runner...'
						: 'GitHub Actions está aplicando el parche y generando el ZIP completo...'
				emitProgress('release', simulatedPercent, phaseMsg)
				continue
			}

			if (matchedRun.conclusion !== 'success') {
				await cleanupStaging()
				throw new Error(
					`GitHub Actions falló al ensamblar el parche (${matchedRun.conclusion || 'error'}). Revisa: ${matchedRun.html_url || `https://github.com/${repo}/actions`}`,
				)
			}
			break
		}

		if (!matchedRun || matchedRun.status !== 'completed') {
			throw new Error('Tiempo de espera agotado esperando a que GitHub Actions termine de ensamblar el parche.')
		}

		emitProgress('upload_manifest', 94, 'Verificando release y manifest.json generados en GitHub...')
		const verifiedRelease = await githubApiRequest(
			`/repos/${repo}/releases/tags/${encodeURIComponent(tagName)}`,
			token,
		)
		const verifiedAssets = Array.isArray(verifiedRelease?.assets) ? verifiedRelease.assets : []
		const zipAsset = verifiedAssets.find((a) => a && a.name === zipFileName)
		const manifestAsset = verifiedAssets.find((a) => a && a.name === 'manifest.json')
		if (!zipAsset || zipAsset.state !== 'uploaded') {
			throw new Error(
				`GitHub Actions terminó, pero no confirmó ${zipFileName} en la release ${tagName}.`,
			)
		}
		if (!manifestAsset || manifestAsset.state !== 'uploaded') {
			throw new Error(
				`GitHub Actions terminó, pero no confirmó manifest.json en la release ${tagName}.`,
			)
		}

		const finalManifest = await downloadJsonWithRedirects(manifestDownloadUrl)
		const finalZipSize = Number(finalManifest?.zip?.size || zipAsset.size || 0)
		const finalZipSha256 = String(finalManifest?.zip?.sha256 || '')
		const finalFilesCount = Array.isArray(finalManifest?.files)
			? finalManifest.files.length
			: collectedFiles.length
		const sizeMb =
			finalZipSize > 0 ? Math.max(0.1, Math.round((finalZipSize / (1024 * 1024)) * 10) / 10) : 0

		const versionFragment = {
			id: `v${cleanVersion}`,
			name: title || `${cleanPackId} v${cleanVersion}`,
			game_version: minecraftVersion,
			loader,
			...(loaderVersion ? { loader_version: loaderVersion } : {}),
			modpack: zipDownloadUrl,
			manifest: manifestDownloadUrl,
			size: finalZipSize,
			size_mb: sizeMb,
			sha256: finalZipSha256,
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

		emitProgress('done', 95, 'Parche ensamblado y verificado en GitHub. Publicando versions.json...')

		return {
			tag_name: tagName,
			archive_name: zipFileName,
			release_url: verifiedRelease.html_url || `https://github.com/${repo}/releases/tag/${tagName}`,
			archive_url: zipDownloadUrl,
			manifest_url: manifestDownloadUrl,
			size: finalZipSize,
			size_mb: sizeMb,
			sha256: finalZipSha256,
			files_count: finalFilesCount,
			output_zip_path: null,
			output_manifest_path: null,
			version_fragment: versionFragment,
		}
	} catch (err) {
		if (!dispatchedToActions) {
			await cleanupStaging()
		}
		throw err
	}
}

async function publishModpack(options) {
	const {
		packId,
		version,
		srcPath,
		repo = 'ramirezjeider28-alt/modpack-versions',
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
		onProgress = null,
		baseManifestUrl = null,
		baseTag = null,
		fastPatch = true,
	} = options

	const emitProgress = (stage, percent, message, extra = {}) => {
		if (typeof onProgress === 'function') {
			onProgress({ stage, percent, message, ...extra })
		}
	}

	if (!packId) throw new Error('--pack <pack_id> is required')
	if (!version) throw new Error('--version <semver> is required')
	if (!srcPath) {
		throw new Error('--src path is required')
	}

	const cleanPackId = String(packId).trim().toLowerCase()
	const cleanVersion = String(version).trim().replace(/^v/i, '')
	const tagName = `${cleanPackId}-v${cleanVersion.replace(/[^a-zA-Z0-9.-]+/g, '-')}`
	const zipFileName = `${cleanPackId}-${cleanVersion}.zip`
	const zipDownloadUrl = `https://github.com/${repo}/releases/download/${tagName}/${zipFileName}`
	const manifestDownloadUrl = `https://github.com/${repo}/releases/download/${tagName}/manifest.json`

	const rules = loadAndValidateRules(cleanPackId, rulesPath)

	emitProgress('extract', 5, 'Preparando y analizando archivos del modpack...')

	const rawSources = (Array.isArray(srcPath) ? srcPath : String(srcPath).split('|'))
		.map((s) => s.trim())
		.filter(Boolean)

	if (rawSources.length === 0) {
		throw new Error('--src path is required')
	}

	for (const s of rawSources) {
		if (!fs.existsSync(s)) {
			throw new Error(`El origen seleccionado no existe: ${s}`)
		}
	}

	const isSingleZip =
		rawSources.length === 1 &&
		fs.statSync(rawSources[0]).isFile() &&
		/\.(zip|mrpack)$/i.test(rawSources[0])
	const isInputZip = isSingleZip

	const createdWorkDirTemp = !outDir
	const workDir = outDir
		? path.resolve(outDir)
		: fs.mkdtempSync(path.join(os.tmpdir(), `dr3sh-pack-${cleanPackId}-`))
	fs.mkdirSync(workDir, { recursive: true })

	let outputZipPath = path.join(workDir, zipFileName)
	const outputManifestPath = path.join(workDir, 'manifest.json')

	try {
		let collectedFiles = null
		let zipSize = 0
		let zipSha256 = ''
		let zipRepackedFromZip = false
		let repackedEntryCrcs = null
		let usedDirectZip = false

		if (isInputZip) {
			const prepared = await prepareInputZip(path.resolve(rawSources[0]), outputZipPath, rules, emitProgress)
			usedDirectZip = prepared.usedDirectZip
			outputZipPath = prepared.outputZipPath
			zipSize = prepared.zipSize
			zipSha256 = prepared.zipSha256
			collectedFiles = prepared.collectedFiles
			repackedEntryCrcs = prepared.entryCrcs
			zipRepackedFromZip = !usedDirectZip
			emitProgress('pack', 23, 'Generando manifest.json...')
		} else {
			emitProgress('pack', 5, 'Analizando orígenes seleccionados...')
			collectedFiles = await collectFromSources(rawSources, rules, emitProgress)
			if (!collectedFiles || collectedFiles.length === 0) {
				throw new Error('No se encontraron archivos válidos para empaquetar en los orígenes seleccionados.')
			}

			if (
				!dryRun &&
				fastPatch !== false &&
				baseManifestUrl &&
				baseTag &&
				String(baseTag).trim() !== tagName
			) {
				const patchResult = await publishModpackViaPatchAction({
					cleanPackId,
					cleanVersion,
					tagName,
					zipFileName,
					zipDownloadUrl,
					manifestDownloadUrl,
					repo,
					minecraftVersion,
					loader,
					loaderVersion,
					title,
					description,
					versionsFile,
					explicitToken,
					workDir,
					collectedFiles,
					baseManifestUrl: String(baseManifestUrl).trim(),
					baseTag: String(baseTag).trim(),
					emitProgress,
				})
				if (patchResult) {
					return patchResult
				}
			}

			let packed = null
			if (archiver) {
				emitProgress('pack', 15, 'Empaquetando con archiver (streaming)...')
				try {
					packed = await writeZipWithArchiver(collectedFiles, outputZipPath, emitProgress)
				} catch (err) {
					console.warn('Archiver stream error, falling back to deterministic writer:', err)
				}
			}
			if (!packed) {
				emitProgress('pack', 16, 'Empaquetando archivos...')
				packed = writeZipDeterministic(collectedFiles, outputZipPath)
			}
			zipSize = packed.size
			zipSha256 = packed.sha256
		}

		const manifest = {
			schema: 1,
			pack_id: cleanPackId,
			version: cleanVersion,
			minecraft_version: minecraftVersion,
			loader,
			...(loaderVersion ? { loader_version: loaderVersion } : {}),
			zip: {
				url: zipDownloadUrl,
				size: zipSize,
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

		if (!usedDirectZip) {
			emitProgress('pack', 24, 'Verificando cabeceras e integridad del paquete ZIP...')
			selfCheckZipHeadersAgainstManifest(outputZipPath, manifest, repackedEntryCrcs || new Map())
		}

		const sizeMb =
			zipSize > 0
				? Math.max(0.1, Math.round((zipSize / (1024 * 1024)) * 10) / 10)
				: 0
		const versionFragment = {
			id: `v${cleanVersion}`,
			name: title || `${cleanPackId} v${cleanVersion}`,
			game_version: minecraftVersion,
			loader,
			...(loaderVersion ? { loader_version: loaderVersion } : {}),
			modpack: zipDownloadUrl,
			manifest: manifestDownloadUrl,
			size: zipSize,
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

			emitProgress('release', 25, `Creando/actualizando release ${tagName} en GitHub...`)

			let release
			try {
				release = await githubApiRequest(
					`/repos/${repo}/releases/tags/${encodeURIComponent(tagName)}`,
					token,
				)
			} catch (err) {
				if (err && err.statusCode && err.statusCode !== 404) {
					throw err
				}
				try {
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
				} catch (createErr) {
					if (createErr && createErr.statusCode === 422) {
						const releases = await githubApiRequest(`/repos/${repo}/releases?per_page=30`, token)
						const existingDraft = Array.isArray(releases)
							? releases.find((r) => r && r.tag_name === tagName)
							: null
						if (!existingDraft) throw createErr
						if (existingDraft.draft) {
							release = await githubApiRequest(
								`/repos/${repo}/releases/${existingDraft.id}`,
								token,
								{
									method: 'PATCH',
									headers: { 'Content-Type': 'application/json' },
									body: JSON.stringify({ draft: false }),
								},
							)
						} else {
							release = existingDraft
						}
					} else {
						throw createErr
					}
				}
			}

			releaseUrl = release.html_url || releaseUrl

			const deleteExistingAssetsByName = async (assetNames) => {
				try {
					const fresh = await githubApiRequest(`/repos/${repo}/releases/${release.id}`, token)
					const existingAssets = fresh.assets || []
					for (const assetName of assetNames) {
						const matches = existingAssets.filter((a) => a.name === assetName)
						for (const match of matches) {
							try {
								await githubApiRequest(`/repos/${repo}/releases/assets/${match.id}`, token, {
									method: 'DELETE',
								})
							} catch {}
						}
					}
				} catch {}
			}

			await deleteExistingAssetsByName([zipFileName, 'manifest.json'])

			const uploadWithRetry = async (filePath, fileName, onByteProgress) => {
				const maxAttempts = 3
				for (let attempt = 1; attempt <= maxAttempts; attempt++) {
					try {
						return await uploadReleaseAsset(
							release.upload_url,
							token,
							filePath,
							fileName,
							onByteProgress,
						)
					} catch (upErr) {
						const status = upErr && upErr.statusCode
						const nonRetryable = status && status >= 400 && status < 500 && status !== 422 && status !== 429
						if (attempt >= maxAttempts || nonRetryable) {
							throw upErr
						}
						await deleteExistingAssetsByName([fileName])
						emitProgress(
							'upload_zip',
							28,
							`Reintentando subida de ${fileName} (intento ${attempt + 1}/${maxAttempts})...`,
						)
						await new Promise((r) => setTimeout(r, 1500 * attempt))
					}
				}
			}

			await uploadWithRetry(outputZipPath, zipFileName, (progress) => {
				const { uploadedBytes, totalBytes, speedBps, etaSeconds, waitingAck } = progress
				const ratio = totalBytes > 0 ? uploadedBytes / totalBytes : 0
				const percent = waitingAck ? 90 : Math.min(89, 28 + Math.round(ratio * 61))
				const msg = waitingAck
					? `Subiendo ${zipFileName} (esperando confirmación de GitHub...)`
					: `Subiendo ${zipFileName} a GitHub Releases...`
				emitProgress('upload_zip', percent, msg, {
					uploadedBytes,
					totalBytes,
					speedBps,
					etaSeconds,
				})
			})
			emitProgress('upload_manifest', 92, 'Subiendo manifest.json a GitHub Releases...')
			await uploadWithRetry(outputManifestPath, 'manifest.json')
			emitProgress('upload_manifest', 94, 'Verificando release y archivos subidos en GitHub...')
			let verifiedRelease = await githubApiRequest(`/repos/${repo}/releases/${release.id}`, token)
			if (verifiedRelease && verifiedRelease.draft) {
				verifiedRelease = await githubApiRequest(
					`/repos/${repo}/releases/${verifiedRelease.id}`,
					token,
					{
						method: 'PATCH',
						headers: { 'Content-Type': 'application/json' },
						body: JSON.stringify({ draft: false }),
					},
				)
			}
			if (verifiedRelease && verifiedRelease.html_url) {
				releaseUrl = verifiedRelease.html_url
			}
			const verifiedAssets = Array.isArray(verifiedRelease?.assets) ? verifiedRelease.assets : []
			const zipAsset = verifiedAssets.find((a) => a && a.name === zipFileName)
			const manifestAsset = verifiedAssets.find((a) => a && a.name === 'manifest.json')
			if (!zipAsset || zipAsset.state !== 'uploaded') {
				throw new Error(
					`GitHub no confirmó el archivo ${zipFileName} en la release ${tagName} (estado: ${zipAsset ? zipAsset.state : 'no encontrado'}).`,
				)
			}
			if (!manifestAsset || manifestAsset.state !== 'uploaded') {
				throw new Error(
					`GitHub no confirmó manifest.json en la release ${tagName} (estado: ${manifestAsset ? manifestAsset.state : 'no encontrado'}).`,
				)
			}
			emitProgress('done', 95, 'Release subida y verificada en GitHub. Publicando versions.json...')
		}

		return {
			tag_name: tagName,
			archive_name: zipFileName,
			release_url: releaseUrl,
			archive_url: zipDownloadUrl,
			manifest_url: manifestDownloadUrl,
			size: zipSize,
			size_mb: sizeMb,
			sha256: zipSha256,
			files_count: collectedFiles.length,
			output_zip_path: outputZipPath,
			output_manifest_path: outputManifestPath,
			version_fragment: versionFragment,
		}
	} finally {
		if (createdWorkDirTemp && !dryRun) {
			try {
				fs.rmSync(workDir, { recursive: true, force: true })
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
	const repo = getArg('--repo') || 'ramirezjeider28-alt/modpack-versions'
	const rulesPath = getArg('--rules')
	const versionsFile = getArg('--versions-file')
	const outDir = getArg('--out-dir') || getArg('--out')
	const minecraftVersion = getArg('--mc') || getArg('--mc-version') || '1.21.1'
	const loader = getArg('--loader') || 'fabric'
	const loaderVersion = getArg('--loader-version')
	const title = getArg('--title')
	const description = getArg('--description') || ''
	const baseManifestUrl = getArg('--base-manifest')
	const baseTag = getArg('--base-tag')
	const fastPatch = !hasFlag('--no-fast-patch')
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
		baseManifestUrl,
		baseTag,
		fastPatch,
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
