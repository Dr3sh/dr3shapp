#!/usr/bin/env node
const { publishModpack } = require('./publish_modpack.cjs')

async function publishRelease({
	token,
	serverId,
	version,
	title,
	archivePath,
	description = '',
	targetRepo = 'Dr3sh/modpack-versions',
}) {
	return await publishModpack({
		packId: serverId,
		version,
		srcPath: archivePath,
		repo: targetRepo,
		title,
		description,
		token,
		dryRun: false,
	})
}

if (require.main === module) {
	const args = process.argv.slice(2)
	const getArg = (flag) => {
		const idx = args.indexOf(flag)
		return idx !== -1 && args[idx + 1] ? args[idx + 1] : ''
	}

	const serverId = getArg('--server') || getArg('--pack')
	const version = getArg('--version')
	const archivePath = getArg('--zip') || getArg('--src')
	const title = getArg('--title')
	const description = getArg('--description')
	const repo = getArg('--repo') || 'Dr3sh/modpack-versions'
	const token = process.env.GITHUB_TOKEN || getArg('--token')

	publishRelease({
		token,
		serverId,
		version,
		title,
		archivePath,
		description,
		targetRepo: repo,
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
