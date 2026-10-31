import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { get_full_path } from '@/helpers/instance'
import { install_create_instance } from '@/helpers/install'
import { resolveImageUrl, type Dr3shServer, type Dr3shVersion } from './use-dr3sh-launcher'

export interface DownloadProgress {
	phase: 'idle' | 'preparing' | 'downloading' | 'extracting' | 'finishing'
	percent: number
	phaseText: string
	downloadedBytes: number
	totalBytes: number
	downloadedFormatted: string
	totalFormatted: string
	speedFormatted: string
}

const isDownloading = ref(false)
const currentServerId = ref<string | null>(null)
const downloadProgress = ref<DownloadProgress>({
	phase: 'idle',
	percent: 0,
	phaseText: '',
	downloadedBytes: 0,
	totalBytes: 0,
	downloadedFormatted: '',
	totalFormatted: '',
	speedFormatted: '',
})

// Reactive map of installed versions so Vue computed properties update immediately
function loadInitialInstalledVersions(): Record<string, string> {
	const map: Record<string, string> = {}
	try {
		for (let i = 0; i < localStorage.length; i++) {
			const key = localStorage.key(i)
			if (key && key.startsWith('dr3sh.installed_version.')) {
				const serverId = key.slice('dr3sh.installed_version.'.length)
				const val = localStorage.getItem(key)
				if (serverId && val) {
					map[serverId] = val
				}
			}
		}
	} catch {}
	return map
}

export const installedVersionsMap = ref<Record<string, string>>(loadInitialInstalledVersions())

export function getInstalledVersion(serverId: string): string | null {
	if (!serverId) return null
	const fromMap = installedVersionsMap.value[serverId]
	if (fromMap) return fromMap
	try {
		const raw = localStorage.getItem(`dr3sh.installed_version.${serverId}`)
		if (raw) {
			installedVersionsMap.value[serverId] = raw
			return raw
		}
	} catch {}
	return null
}

export function getInstalledFolder(serverId: string, serverTitle?: string): string | null {
	try {
		const raw = localStorage.getItem(`dr3sh.installed_folder.${serverId}`) || null
		if (!raw) return null
		const leaf = raw.split(/[\\/]/).filter(Boolean).pop()?.toLowerCase().trim() || ''
		const normTitle = (serverTitle || '').toLowerCase().trim()
		const normId = serverId.toLowerCase().trim()
		if (normTitle && leaf && leaf !== normTitle && leaf !== normId) {
			const numTitle = normTitle.match(/\d+$/)?.[0]
			const numLeaf = leaf.match(/\d+$/)?.[0]
			if (numTitle !== numLeaf && (numTitle || numLeaf)) {
				localStorage.removeItem(`dr3sh.installed_folder.${serverId}`)
				return null
			}
		}
		return raw
	} catch {
		return null
	}
}

export function getInstalledInstanceId(serverId: string): string | null {
	try {
		return localStorage.getItem(`dr3sh.installed_instance_id.${serverId}`) || null
	} catch {
		return null
	}
}

export function findMatchingInstance(server: Dr3shServer | null | undefined, instancesList: any[]): any | null {
	if (!server || !Array.isArray(instancesList) || instancesList.length === 0) return null
	const norm = (s: unknown) =>
		String(s || '')
			.toLowerCase()
			.replace(/[\s_-]+/g, ' ')
			.trim()

	const targetId = norm(server.id)
	const targetTitle = norm(server.title)
	const validNames = new Set<string>([targetId, targetTitle])
	if (Array.isArray(server.aliases)) {
		for (const alias of server.aliases) {
			if (alias) validNames.add(norm(alias))
		}
	}
	if (server.id === 'cobbleverse') {
		validNames.add('desgraciamon')
		validNames.add('desgraciamon 1')
	}

	// 1. Check explicitly bound instance ID first, verifying its name matches this server (not Desgraciamon 2/4)
	const savedInstId = getInstalledInstanceId(server.id)
	if (savedInstId) {
		const byId = instancesList.find(
			(inst: any) =>
				(inst.id === savedInstId || inst.path === savedInstId) &&
				validNames.has(norm(inst.name)),
		)
		if (byId) return byId
	}

	// 2. Strict exact match by normalized name or folder path (NEVER substring .includes()!)
	return (
		instancesList.find((inst: any) => {
			const instName = norm(inst.name)
			const instPath = norm(inst.path)
			return validNames.has(instName) || validNames.has(instPath)
		}) || null
	)
}

export function setInstalledVersion(serverId: string, version: string, folder?: string, instanceId?: string) {
	try {
		localStorage.setItem(`dr3sh.installed_version.${serverId}`, version)
		if (folder) {
			localStorage.setItem(`dr3sh.installed_folder.${serverId}`, folder)
		}
		if (instanceId) {
			localStorage.setItem(`dr3sh.installed_instance_id.${serverId}`, instanceId)
		}
	} catch {}
	installedVersionsMap.value = {
		...installedVersionsMap.value,
		[serverId]: version,
	}
}

export function removeInstalledVersion(serverId: string) {
	try {
		localStorage.removeItem(`dr3sh.installed_version.${serverId}`)
		localStorage.removeItem(`dr3sh.installed_folder.${serverId}`)
		localStorage.removeItem(`dr3sh.installed_instance_id.${serverId}`)
	} catch {}
	const next = { ...installedVersionsMap.value }
	delete next[serverId]
	installedVersionsMap.value = next
}

export async function cancelActiveDownload() {
	try {
		await invoke('dr3sh_cancel_download')
	} catch (e) {
		console.warn('Error cancelling download:', e)
	}
}

let isReconcilingOrphans = false

export async function reconcileOrphanedInstalledInstances(
	serversList: Dr3shServer[],
	instancesList: any[],
): Promise<boolean> {
	if (isReconcilingOrphans || isDownloading.value) return false
	isReconcilingOrphans = true
	let changed = false
	try {
		for (const server of serversList) {
			const installedVer = getInstalledVersion(server.id)
			const savedFolder = getInstalledFolder(server.id, server.title)
			if (!installedVer || !savedFolder) continue

			const existingMatch = findMatchingInstance(server, instancesList)
			if (existingMatch) continue

			const hasMods = await invoke<boolean>('dr3sh_has_extracted_mods', {
				folder: savedFolder,
			}).catch(() => false)
			if (!hasMods) continue

			const gameVersion = server.minecraft_version || '1.21.1'
			const rawLoader = (server.mod_loader || 'fabric').toLowerCase()
			const modloader = (['fabric', 'forge', 'neoforge', 'quilt'].includes(rawLoader)
				? rawLoader
				: 'fabric') as any
			const loaderVersion = server.mod_loader_version || null

			let snap: any = null
			try {
				snap = await install_create_instance({
					name: server.title,
					gameVersion,
					loader: modloader,
					loaderVersion,
					iconPath: null,
				})
			} catch {
				snap = await install_create_instance({
					name: server.title,
					gameVersion,
					loader: modloader,
					loaderVersion: null,
					iconPath: null,
				}).catch(() => null)
			}

			const newInstId = snap?.instance_id || snap?.id || null
			if (newInstId) {
				await invoke('dr3sh_wait_for_instance_ready', { instanceId: newInstId }).catch(() => {})
				const instDir = await get_full_path(newInstId).catch(() => null)
				if (instDir) {
					await invoke('dr3sh_link_existing_instance', {
						instanceId: newInstId,
						instanceDir: instDir,
						existingFolder: savedFolder,
					}).catch(() => {})
				}
				setInstalledVersion(server.id, installedVer, savedFolder, newInstId)
				changed = true
			}
		}
	} catch (e) {
		console.warn('Error reconciling orphaned installed instances:', e)
	} finally {
		isReconcilingOrphans = false
	}
	return changed
}

export function useDr3shDownloader() {
	async function downloadAndInstallModpack(options: {
		server: Dr3shServer
		version: Dr3shVersion
		matchingInstanceId?: string | null
		customFolder?: string | null
		onSuccess?: () => void
		onError?: (err: any) => void
	}) {
		const { server, version, matchingInstanceId, customFolder, onSuccess, onError } = options

		if (isDownloading.value) {
			console.warn('A download is already in progress.')
			return
		}

		const downloadUrl = version.link
		if (!downloadUrl) {
			const err = new Error(`La versión ${version.version || ''} no tiene enlace de descarga disponible.`)
			if (onError) onError(err)
			throw err
		}

		isDownloading.value = true
		currentServerId.value = server.id
		downloadProgress.value = {
			phase: 'preparing',
			percent: 2,
			phaseText: `Preparando ${server.title}...`,
			downloadedBytes: 0,
			totalBytes: (version.size_mb || 220) * 1024 * 1024,
			downloadedFormatted: '0 MB',
			totalFormatted: `${version.size_mb || 220} MB`,
			speedFormatted: '',
		}

		let unlisten: UnlistenFn | null = null

		try {
			// Listen for real-time Rust progress events
			unlisten = await listen<any>('dr3sh-download-progress', (event) => {
				const p = event.payload
				if (!p || p.serverId !== server.id) return
				// Keep phaseText short and clean without technical clutter
				const rawText = String(p.phaseText || downloadProgress.value.phaseText)
				const cleanText = rawText
					.replace(/\s*\([^)]*\/[^)]*\)\s*/g, '')
					.replace(/\s*•.*$/g, '')
					.trim()
				downloadProgress.value = {
					phase: p.phase || 'downloading',
					percent: typeof p.percent === 'number' ? p.percent : downloadProgress.value.percent,
					phaseText: cleanText || `Descargando ${server.title}...`,
					downloadedBytes: p.downloadedBytes ?? 0,
					totalBytes: p.totalBytes ?? 0,
					downloadedFormatted: p.downloadedFormatted || '0 MB',
					totalFormatted: p.totalFormatted || '',
					speedFormatted: p.speedFormatted || '',
				}
			})

			// 1. Ensure instance exists or create it in Theseus
			let targetInstanceId = matchingInstanceId || null
			if (!targetInstanceId) {
				await invoke('dr3sh_cleanup_deleted_instance', { customFolder: null }).catch(() => {})
				const finalName = server.title
				const gameVersion = version.minecraft_version || server.minecraft_version || '1.21.1'
				const rawLoader = (version.mod_loader || server.mod_loader || 'fabric').toLowerCase()
				const modloader = (['fabric', 'forge', 'neoforge', 'quilt'].includes(rawLoader)
					? rawLoader
					: 'fabric') as any
				const loaderVersion = version.mod_loader_version || server.mod_loader_version || null
				const rawIcon = server.logo_square || server.logo || server.image || ''
				const resolvedIcon = rawIcon ? resolveImageUrl(rawIcon) : null

				let snap: any = null
				try {
					snap = await install_create_instance({
						name: finalName,
						gameVersion,
						loader: modloader,
						loaderVersion,
						iconPath: resolvedIcon && resolvedIcon.startsWith('http') ? resolvedIcon : null,
					})
				} catch (firstErr) {
					console.warn('Retrying instance creation with automatic loader version & null icon:', firstErr)
					snap = await install_create_instance({
						name: finalName,
						gameVersion,
						loader: modloader,
						loaderVersion: null,
						iconPath: null,
					})
				}

				targetInstanceId = snap?.instance_id || snap?.id || null
			}

			if (!targetInstanceId) {
				throw new Error('No se pudo crear el perfil de la instancia en el launcher.')
			}

			// 2. Wait for Theseus base instance setup (Minecraft + Loader) to finish BEFORE modifying or linking the folder
			downloadProgress.value = {
				...downloadProgress.value,
				phase: 'preparing',
				percent: 2,
				phaseText: `Preparando perfil de ${server.title}...`,
				speedFormatted: '',
			}
			try {
				await Promise.race([
					invoke('dr3sh_wait_for_instance_ready', { instanceId: targetInstanceId }),
					new Promise((resolve) => setTimeout(resolve, 45000)),
				])
			} catch (waitErr) {
				console.warn('Initial wait for instance ready warning:', waitErr)
			}

			// 3. Resolve the Theseus instance path
			const instanceDir = await get_full_path(targetInstanceId)
			if (!instanceDir) {
				throw new Error('No se pudo determinar el directorio de instalación para la instancia.')
			}

			// 4. Execute high-speed native Rust download & delta extraction
			downloadProgress.value = {
				...downloadProgress.value,
				phase: 'downloading',
				percent: 2,
				phaseText: `Conectando con el servidor de descarga...`,
				speedFormatted: 'Conectando...',
			}
			const finalInstalledPath = await invoke<string>('dr3sh_download_modpack', {
				serverId: server.id,
				serverTitle: server.title,
				downloadUrl,
				manifestUrl: version.manifest ? resolveImageUrl(version.manifest) : null,
				instanceDir,
				customDir: customFolder?.trim() || null,
				expectedSizeMb: version.size_mb || 220,
				expectedSha256: version.sha256 || null,
				instanceId: targetInstanceId,
			})

			// 5. Ensure any post-extraction Theseus job has settled
			downloadProgress.value = {
				...downloadProgress.value,
				phase: 'finishing',
				percent: 99,
				phaseText: 'Finalizando instalación...',
				speedFormatted: '',
			}
			try {
				await Promise.race([
					invoke('dr3sh_wait_for_instance_ready', { instanceId: targetInstanceId }),
					new Promise((resolve) => setTimeout(resolve, 20000)),
				])
			} catch (waitErr) {
				console.warn('Wait for instance ready warning:', waitErr)
			}

			// 5. Mark installed version reactively
			downloadProgress.value = {
				...downloadProgress.value,
				phase: 'finishing',
				percent: 100,
				phaseText: '¡Instalación completada!',
				speedFormatted: '',
			}

			setInstalledVersion(
				server.id,
				version.version || '1.0.0',
				finalInstalledPath || instanceDir,
				targetInstanceId,
			)

			if (onSuccess) await onSuccess()
		} catch (err: any) {
			console.error('Error during download & install pipeline:', err)
			if (onError) onError(err)
			throw err
		} finally {
			if (unlisten) {
				unlisten()
			}
			isDownloading.value = false
			currentServerId.value = null
			setTimeout(() => {
				if (!isDownloading.value) {
					downloadProgress.value.phase = 'idle'
				}
			}, 2500)
		}
	}

	return {
		isDownloading,
		currentServerId,
		downloadProgress,
		installedVersionsMap,
		getInstalledVersion,
		getInstalledFolder,
		getInstalledInstanceId,
		findMatchingInstance,
		setInstalledVersion,
		removeInstalledVersion,
		cancelDownload: cancelActiveDownload,
		downloadAndInstallModpack,
	}
}
