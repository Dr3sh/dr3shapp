#!/usr/bin/env node
const { publishModpack } = require('./publish_modpack.cjs')

async function publishRelease({
	token,
	serverId,
	version,
	title,
	archivePath,
	description = '',
	targetRepo = 'ramirezjeider28-alt/modpack-versions',
	minecraftVersion = '1.21.1',
	loader = 'fabric',
	loaderVersion = '',
	baseManifestUrl = '',
	baseTag = '',
	fastPatch = true,
}) {
	return await publishModpack({
		packId: serverId,
		version,
		srcPath: archivePath,
		repo: targetRepo,
		minecraftVersion: minecraftVersion || '1.21.1',
		loader: loader || 'fabric',
		loaderVersion: loaderVersion || null,
		title,
		description,
		token,
		baseManifestUrl: baseManifestUrl || null,
		baseTag: baseTag || null,
		fastPatch,
		dryRun: false,
		onProgress: (progress) => {
			console.log(`DR3SH_PROGRESS:${JSON.stringify(progress)}`)
		},
	})
}

if (require.main === module) {
	const args = process.argv.slice(2)
	const getArg = (flag) => {
		const idx = args.indexOf(flag)
		return idx !== -1 && args[idx + 1] && !args[idx + 1].startsWith('--') ? args[idx + 1] : ''
	}

	const serverId = getArg('--server') || getArg('--pack')
	const version = getArg('--version')
	const archivePath = getArg('--zip') || getArg('--src')
	const title = getArg('--title')
	const description = getArg('--description')
	const repo = getArg('--repo') || 'ramirezjeider28-alt/modpack-versions'
	const minecraftVersion = getArg('--mc') || getArg('--mc-version') || '1.21.1'
	const loader = getArg('--loader') || 'fabric'
	const loaderVersion = getArg('--loader-version')
	const baseManifestUrl = getArg('--base-manifest')
	const baseTag = getArg('--base-tag')
	const fastPatch = !args.includes('--no-fast-patch')
	const token = process.env.GITHUB_TOKEN || getArg('--token')

	publishRelease({
		token,
		serverId,
		version,
		title,
		archivePath,
		description,
		targetRepo: repo,
		minecraftVersion,
		loader,
		loaderVersion,
		baseManifestUrl,
		baseTag,
		fastPatch,
	})
		.then((result) => {
			console.log(`DR3SH_RESULT_JSON_START${JSON.stringify(result)}DR3SH_RESULT_JSON_END`)
			process.exit(0)
		})
		.catch((err) => {
			console.error(err.message || String(err))
			process.exit(1)
		})
}

module.exports = { publishRelease }
