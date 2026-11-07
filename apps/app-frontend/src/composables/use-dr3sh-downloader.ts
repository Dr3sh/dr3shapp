import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { get_full_path } from '@/helpers/instance'
import { install_create_instance } from '@/helpers/install'
import { resolveImageUrl, type Dr3shServer, type Dr3shVersion } from './use-dr3sh-launcher'

export interface DownloadProgress {
	phase: 'idle' | 'preparing' | 'downloading' | 'verifying' | 'extracting' | 'applying' | 'finishing'
	percent: number
	phaseText: string
	downloadedBytes: number
	totalBytes: number
	downloadedFormatted: string
	totalFormatted: string
	speedFormatted: string
	etaSeconds?: number | null
	attempt?: number
}

export interface Dr3shLastError {
	code: string
	op_id: string
	message: string
	details: string
}

export interface Dr3shServerStateEntry {
	server_id: string
	instance_id: string | null
	instance_path: string | null
	installed_version: string | null
	state:
		| 'not_installed'
		| 'preparing'
		| 'downloading'
		| 'verifying'
		| 'extracting'
		| 'applying'
		| 'installed'
		| 'recovering'
		| 'error'
	last_error: Dr3shLastError | null
	migration_notice_pending: boolean
	locked: boolean
	active_op_id: string | null
	updated_at: string
}

const isDownloading = ref(false)
const currentServerId = ref<string | null>(null)
const currentOpId = ref<string | null>(null)
const lastOperationError = ref<Dr3shLastError | null>(null)
const serverStatesMap = ref<Record<string, Dr3shServerStateEntry>>({})
export const installedVersionsMap = ref<Record<string, string>>({})

const downloadProgress = ref<DownloadProgress>({
	phase: 'idle',
	percent: 0,
	phaseText: '',
	downloadedBytes: 0,
	totalBytes: 0,
	downloadedFormatted: '',
	totalFormatted: '',
	speedFormatted: '',
	etaSeconds: null,
	attempt: 0,
})

function formatBytesUi(bytes: number): string {
	if (!bytes || bytes <= 0) return '0 MB'
	const gb = 1024 * 1024 * 1024
	const mb = 1024 * 1024
	if (bytes >= gb) return `${(bytes / gb).toFixed(2)} GB`
	if (bytes >= mb) return `${(bytes / mb).toFixed(1)} MB`
	return `${Math.round(bytes / 1024)} KB`
}

function syncMapsFromState(states: Record<string, Dr3shServerStateEntry>) {
	serverStatesMap.value = states
	const nextVersions: Record<string, string> = {}
	for (const [sid, entry] of Object.entries(states)) {
		if (entry && entry.installed_version) {
			nextVersions[sid] = entry.installed_version
		}
	}
	installedVersionsMap.value = nextVersions
}

let stateInitialized = false
let globalStateUnlisten: UnlistenFn | null = null

export async function initDr3shBackendState(): Promise<Record<string, Dr3shServerStateEntry>> {
	if (!stateInitialized) {
		stateInitialized = true
		try {
			const legacyEntries: Record<
				string,
				{
					installed_version?: string | null
					installed_folder?: string | null
					installed_instance_id?: string | null
				}
			> = {}
			const keysToRemove: string[] = []
			for (let i = 0; i < localStorage.length; i++) {
				const key = localStorage.key(i)
				if (!key) continue
				if (key.startsWith('dr3sh.installed_version.')) {
					const sid = key.slice('dr3sh.installed_version.'.length)
					legacyEntries[sid] = legacyEntries[sid] || {}
					legacyEntries[sid].installed_version = localStorage.getItem(key)
					keysToRemove.push(key)
				} else if (key.startsWith('dr3sh.installed_folder.')) {
					const sid = key.slice('dr3sh.installed_folder.'.length)
					legacyEntries[sid] = legacyEntries[sid] || {}
					legacyEntries[sid].installed_folder = localStorage.getItem(key)
					keysToRemove.push(key)
				} else if (key.startsWith('dr3sh.installed_instance_id.')) {
					const sid = key.slice('dr3sh.installed_instance_id.'.length)
					legacyEntries[sid] = legacyEntries[sid] || {}
					legacyEntries[sid].installed_instance_id = localStorage.getItem(key)
					keysToRemove.push(key)
				}
			}

			if (Object.keys(legacyEntries).length > 0) {
				const migrated = await invoke<Record<string, Dr3shServerStateEntry>>(
					'dr3sh_migrate_local_state',
					{ entries: legacyEntries },
				)
				for (const k of keysToRemove) {
					localStorage.removeItem(k)
				}
				syncMapsFromState(migrated)
			} else {
				const current = await invoke<Record<string, Dr3shServerStateEntry>>('dr3sh_get_state')
				syncMapsFromState(current)
			}
		} catch (e) {
			console.warn('Failed to initialize dr3sh backend state:', e)
		}

		if (!globalStateUnlisten) {
			listen<Dr3shServerStateEntry>('dr3sh://state', (ev) => {
				const entry = ev.payload
				if (!entry || !entry.server_id) return
				serverStatesMap.value = {
					...serverStatesMap.value,
					[entry.server_id]: entry,
				}
				if (entry.installed_version) {
					installedVersionsMap.value = {
						...installedVersionsMap.value,
						[entry.server_id]: entry.installed_version,
					}
				} else {
					const next = { ...installedVersionsMap.value }
					delete next[entry.server_id]
					installedVersionsMap.value = next
				}
				if (entry.last_error) {
					lastOperationError.value = entry.last_error
				}
			})
				.then((fn) => {
					globalStateUnlisten = fn
				})
				.catch(() => {})
		}
	} else {
		try {
			const current = await invoke<Record<string, Dr3shServerStateEntry>>('dr3sh_get_state')
			syncMapsFromState(current)
		} catch {}
	}
	return serverStatesMap.value
}

void initDr3shBackendState()

export function getServerBackendState(serverId: string): Dr3shServerStateEntry | null {
	if (!serverId) return null
	return serverStatesMap.value[serverId] || null
}

export function getInstalledVersion(serverId: string): string | null {
	if (!serverId) return null
	return (
		serverStatesMap.value[serverId]?.installed_version ||
		installedVersionsMap.value[serverId] ||
		null
	)
}

export function getInstalledFolder(serverId: string): string | null {
	if (!serverId) return null
	return serverStatesMap.value[serverId]?.instance_path || null
}

export function getInstalledInstanceId(serverId: string): string | null {
	if (!serverId) return null
	return serverStatesMap.value[serverId]?.instance_id || null
}

export function isServerReadyToPlay(serverId: string): boolean {
	if (!serverId) return false
	const entry = serverStatesMap.value[serverId]
	if (!entry) return false
	return entry.state === 'installed' && !entry.locked
}

export function findMatchingInstance(
	server: Dr3shServer | null | undefined,
	instancesList: any[],
): any | null {
	if (!server || !Array.isArray(instancesList) || instancesList.length === 0) return null

	const boundId = getInstalledInstanceId(server.id)
	if (boundId) {
		const byBoundId = instancesList.find(
			(inst: any) => inst.id === boundId || inst.path === boundId,
		)
		if (byBoundId) return byBoundId
	}

	if (Array.isArray(server.aliases) && server.aliases.length > 0) {
		const exactAliases = new Set(
			server.aliases.map((a) => String(a || '').trim().toLowerCase()).filter(Boolean),
		)
		const matchedByAlias = instancesList.find((inst: any) => {
			const instName = String(inst.name || '').trim().toLowerCase()
			const instPath = String(inst.path || '').trim().toLowerCase()
			return exactAliases.has(instName) || exactAliases.has(instPath)
		})
		if (matchedByAlias) {
			const matchedId = matchedByAlias.path || matchedByAlias.id
			if (matchedId) {
				setInstalledVersion(
					server.id,
					getInstalledVersion(server.id) || server.version || '1.0.0',
					getInstalledFolder(server.id) || undefined,
					matchedId,
				)
			}
			return matchedByAlias
		}
	}

	return null
}

export function setInstalledVersion(
	serverId: string,
	version: string,
	folder?: string,
	instanceId?: string,
) {
	installedVersionsMap.value = {
		...installedVersionsMap.value,
		[serverId]: version,
	}
	const prev = serverStatesMap.value[serverId]
	serverStatesMap.value = {
		...serverStatesMap.value,
		[serverId]: {
			server_id: serverId,
			instance_id: instanceId ?? prev?.instance_id ?? null,
			instance_path: folder ?? prev?.instance_path ?? null,
			installed_version: version,
			state: 'installed',
			last_error: null,
			migration_notice_pending: prev?.migration_notice_pending ?? false,
			locked: false,
			active_op_id: null,
			updated_at: new Date().toISOString(),
		},
	}
	void invoke('dr3sh_migrate_local_state', {
		entries: {
			[serverId]: {
				installed_version: version,
				installed_folder: folder ?? prev?.instance_path ?? null,
				installed_instance_id: instanceId ?? prev?.instance_id ?? null,
			},
		},
	}).catch(() => {})
}

export function removeInstalledVersion(serverId: string) {
	const next = { ...installedVersionsMap.value }
	delete next[serverId]
	installedVersionsMap.value = next

	const nextStates = { ...serverStatesMap.value }
	delete nextStates[serverId]
	serverStatesMap.value = nextStates
}

export async function cancelActiveDownload(opId?: string | null) {
	try {
		await invoke('dr3sh_cancel', { opId: opId || currentOpId.value || '*' })
	} catch (e) {
		console.warn('Error cancelling operation:', e)
	}
}

export async function getDiagnosticsText(opId?: string | null, serverId?: string | null): Promise<string> {
	return await invoke<string>('dr3sh_get_diagnostics', {
		opId: opId || currentOpId.value || null,
		serverId: serverId || currentServerId.value || null,
	})
}

export async function openLogsFolder(): Promise<void> {
	await invoke('dr3sh_open_logs_folder')
}

export async function acknowledgeMigrationNotice(serverId: string): Promise<void> {
	await invoke('dr3sh_ack_migration_notice', { serverId })
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
		await initDr3shBackendState()
		for (const server of serversList) {
			const installedVer = getInstalledVersion(server.id)
			const savedFolder = getInstalledFolder(server.id)
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
	async function runBackendOperation(options: {
		mode: 'install' | 'repair'
		server: Dr3shServer
		version: Dr3shVersion
		customFolder?: string | null
		bypassGate?: boolean
		onSuccess?: () => void
		onError?: (err: any) => void
	}) {
		const { mode, server, version, customFolder, bypassGate, onSuccess, onError } = options

		if (isDownloading.value) {
			return
		}

		isDownloading.value = true
		currentServerId.value = server.id
		lastOperationError.value = null

		const estTotalBytes = Math.round((version.size_mb || 220) * 1024 * 1024)
		downloadProgress.value = {
			phase: 'preparing',
			percent: 1,
			phaseText:
				mode === 'repair'
					? `Verificando archivos de ${server.title}...`
					: `Preparando ${server.title}...`,
			downloadedBytes: 0,
			totalBytes: estTotalBytes,
			downloadedFormatted: '0 MB',
			totalFormatted: formatBytesUi(estTotalBytes),
			speedFormatted: '',
			etaSeconds: null,
			attempt: 0,
		}

		let unlistenProgress: UnlistenFn | null = null
		let unlistenDone: UnlistenFn | null = null

		try {
			const rawIcon = server.logo_square || server.logo || server.image || ''
			const resolvedIcon = rawIcon ? resolveImageUrl(rawIcon) : null
			const versionRefPayload = {
				id: version.version || '1.0.0',
				name: version.version || server.title,
				serverName: server.title,
				gameVersion: version.minecraft_version || server.minecraft_version || '1.21.1',
				loader: (version.mod_loader || server.mod_loader || 'fabric').toLowerCase(),
				loaderVersion: version.mod_loader_version || server.mod_loader_version || null,
				downloadUrl: version.link || null,
				manifestUrl: version.manifest ? resolveImageUrl(version.manifest) : null,
				sha256: version.sha256 || null,
				sizeMb: version.size_mb || null,
				iconUrl: resolvedIcon && resolvedIcon.startsWith('http') ? resolvedIcon : null,
				bypassGate: Boolean(bypassGate),
			}

			const donePromise = new Promise<any>((resolve, reject) => {
				listen<any>('dr3sh://done', (ev) => {
					const payload = ev.payload
					if (!payload || payload.serverId !== server.id) return
					if (currentOpId.value && payload.opId !== currentOpId.value) return
					if (payload.ok) {
						resolve(payload)
					} else {
						const errPayload: Dr3shLastError = payload.error || {
							code: 'E_UNKNOWN',
							op_id: payload.opId || 'unknown',
							message: 'Error inesperado durante la instalación.',
							details: '',
						}
						lastOperationError.value = errPayload
						const errObj = Object.assign(new Error(errPayload.message), errPayload)
						reject(errObj)
					}
				})
					.then((fn) => {
						unlistenDone = fn
					})
					.catch(reject)
			})

			unlistenProgress = await listen<any>('dr3sh://progress', (ev) => {
				const p = ev.payload
				const pServerId = p?.serverId ?? p?.server_id
				const pOpId = p?.opId ?? p?.op_id
				if (!p || pServerId !== server.id) return
				if (currentOpId.value && pOpId && pOpId !== currentOpId.value) return

				const stageMap: Record<string, DownloadProgress['phase']> = {
					preflight: 'preparing',
					base: 'preparing',
					download: 'downloading',
					verify: 'verifying',
					extract: 'extracting',
					apply: 'applying',
				}
				const mappedPhase = stageMap[p.stage] || p.phase || 'downloading'
				const dlBytes =
					typeof p.downloadedBytes === 'number'
						? p.downloadedBytes
						: typeof p.bytes_done === 'number'
							? p.bytes_done
							: 0
				const totBytes =
					typeof p.totalBytes === 'number' && p.totalBytes > 0
						? p.totalBytes
						: typeof p.bytes_total === 'number' && p.bytes_total > 0
							? p.bytes_total
							: estTotalBytes
				const spdBps =
					typeof p.speedBps === 'number'
						? p.speedBps
						: typeof p.speed_bps === 'number'
							? p.speed_bps
							: 0
				const statusText =
					p.stageText || p.phaseText || p.status_text || downloadProgress.value.phaseText

				downloadProgress.value = {
					phase: mappedPhase,
					percent:
						typeof p.percent === 'number' ? Math.round(p.percent) : downloadProgress.value.percent,
					phaseText: statusText,
					downloadedBytes: dlBytes,
					totalBytes: totBytes,
					downloadedFormatted: p.downloadedFormatted || formatBytesUi(dlBytes),
					totalFormatted: p.totalFormatted || formatBytesUi(totBytes),
					speedFormatted:
						p.speedFormatted || (spdBps > 0 ? `${formatBytesUi(spdBps)}/s` : ''),
					etaSeconds: p.eta_s ?? null,
					attempt: p.retry?.attempt ?? p.attempt ?? 0,
				}
			})

			const opId =
				mode === 'repair'
					? await invoke<string>('dr3sh_repair', {
							serverId: server.id,
							versionRef: versionRefPayload,
						})
					: await invoke<string>('dr3sh_install_or_update', {
							serverId: server.id,
							versionRef: versionRefPayload,
							customDir: customFolder?.trim() || null,
						})

			currentOpId.value = opId

			const doneResult = await donePromise

			downloadProgress.value = {
				...downloadProgress.value,
				phase: 'finishing',
				percent: 100,
				phaseText: '¡Instalación completada!',
				speedFormatted: '',
			}

			await initDr3shBackendState()

			if (doneResult?.version) {
				installedVersionsMap.value = {
					...installedVersionsMap.value,
					[server.id]: doneResult.version,
				}
			}

			if (onSuccess) await onSuccess()
		} catch (err: any) {
			if (onError) onError(err)
			throw err
		} finally {
			if (unlistenProgress) unlistenProgress()
			if (unlistenDone) unlistenDone()
			isDownloading.value = false
			currentServerId.value = null
			currentOpId.value = null
			setTimeout(() => {
				if (!isDownloading.value) {
					downloadProgress.value.phase = 'idle'
				}
			}, 2500)
		}
	}

	async function downloadAndInstallModpack(options: {
		server: Dr3shServer
		version: Dr3shVersion
		matchingInstanceId?: string | null
		customFolder?: string | null
		bypassGate?: boolean
		onSuccess?: () => void
		onError?: (err: any) => void
	}) {
		return await runBackendOperation({
			mode: 'install',
			...options,
		})
	}

	async function repairModpack(options: {
		server: Dr3shServer
		version: Dr3shVersion
		onSuccess?: () => void
		onError?: (err: any) => void
	}) {
		return await runBackendOperation({
			mode: 'repair',
			...options,
		})
	}

	return {
		isDownloading,
		currentServerId,
		currentOpId,
		lastOperationError,
		serverStatesMap,
		downloadProgress,
		installedVersionsMap,
		getServerBackendState,
		getInstalledVersion,
		getInstalledFolder,
		getInstalledInstanceId,
		isServerReadyToPlay,
		findMatchingInstance,
		setInstalledVersion,
		removeInstalledVersion,
		cancelActiveDownload,
		getDiagnosticsText,
		openLogsFolder,
		acknowledgeMigrationNotice,
		downloadAndInstallModpack,
		repairModpack,
	}
}
