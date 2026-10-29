<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openPath, openUrl } from '@tauri-apps/plugin-opener'
import { useQuery } from '@tanstack/vue-query'
import { instanceListQueryOptions } from '@/pages/instance/query-options'
import {
	useDr3shLauncher,
	type Dr3shServer,
	getServerPreviewImages,
	getStageImageUrl,
	getThumbnailUrl,
	handleImageError,
	hasCustomScreenshots,
	loaderIconUrl,
	formatLoaderName,
	deleteConfirmModal,
	requestDeleteConfirm,
	closeDeleteConfirm,
	loginOfflineAccount,
	loginMicrosoftAccount,
	switchActiveAccount,
	removeSavedAccount,
	refreshAccount,
} from '@/composables/use-dr3sh-launcher'
import {
	useDr3shDownloader,
	installedVersionsMap,
	getInstalledVersion,
	getInstalledFolder,
	findMatchingInstance,
	removeInstalledVersion,
	reconcileOrphanedInstalledInstances,
} from '@/composables/use-dr3sh-downloader'
import { run, kill as killInstance, remove, get_full_path } from '@/helpers/instance'
import { install_existing_instance } from '@/helpers/install'
import { get as getSettings, set as setSettings } from '@/helpers/settings'
import { get_max_memory, get_java_versions } from '@/helpers/jre.js'
import { get_by_instance_id } from '@/helpers/process.js'
import { showInstanceInFolder } from '@/helpers/utils.js'
import { handleSevereError } from '@/composables/use-error.js'

defineOptions({
	name: 'Dr3shMainPage',
})

const AIKAR_JVM_FLAGS =
	'-XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:MaxGCPauseMillis=200 -XX:+UnlockExperimentalVMOptions -XX:+DisableExplicitGC -XX:+AlwaysPreTouch -XX:G1NewSizePercent=30 -XX:G1MaxNewSizePercent=40 -XX:G1HeapRegionSize=8M -XX:G1ReservePercent=20 -XX:G1HeapWastePercent=5 -XX:G1MixedGCCountTarget=4 -XX:InitiatingHeapOccupancyPercent=15 -XX:G1MixedGCLiveThresholdPercent=90 -XX:G1RSetUpdatingPauseTimePercent=5 -XX:SurvivorRatio=32 -XX:+PerfDisableSharedMem -XX:MaxTenuringThreshold=1'

const {
	servers,
	versions,
	loading,
	owned,
	selectedId,
	selectedServer,
	selectedVersion,
	currentView,
	activeSettingsTab,
	activeAccount,
	allAccounts,
	maintenanceMode,
	maintenanceMessage,
	testingMode,
	testingHours,
	isDeveloper,
	launcherRefreshMode,
	fetchVersionsData,
	addOwned,
	removeOwned,
	resolveImageUrl,
} = useDr3shLauncher()

const instancesQuery = useQuery(instanceListQueryOptions())
const instances = computed(() => instancesQuery.data.value ?? [])

watch(
	[servers, instances],
	async ([srvList, instList]) => {
		if (!srvList.length || !instancesQuery.isFetched.value) return
		const reconciled = await reconcileOrphanedInstalledInstances(srvList, instList)
		if (reconciled) {
			await instancesQuery.refetch()
		}
	},
	{ immediate: true },
)

const searchQuery = ref('')
const busy = ref(false)
const notice = ref<string | null>(null)
let toastTimer: ReturnType<typeof setTimeout> | null = null

function showToast(msg: string, durationMs = 3800) {
	if (toastTimer) {
		clearTimeout(toastTimer)
		toastTimer = null
	}
	notice.value = msg
	if (durationMs > 0) {
		toastTimer = setTimeout(() => {
			if (notice.value === msg) {
				notice.value = null
			}
			toastTimer = null
		}, durationMs)
	}
}

// 3-dots dropdown menu state
const instanceMenuOpen = ref(false)

function toggleInstanceMenu() {
	instanceMenuOpen.value = !instanceMenuOpen.value
}

function closeInstanceMenu() {
	instanceMenuOpen.value = false
}

// Install location modal state
interface InstallLocationState {
	server: Dr3shServer
	mode: 'default' | 'custom'
	customFolder: string
}

const installLocationModal = ref<InstallLocationState | null>(null)
const defaultInstancesBaseDir = ref<string>('AppData\\Roaming\\Dr3shLauncher\\instances')

// Dr3sh Native Rust Downloader
const {
	isDownloading,
	currentServerId,
	downloadProgress,
	cancelDownload,
	downloadAndInstallModpack,
} = useDr3shDownloader()

// Gallery / Screenshot state
const activeScreenshotIndex = ref(0)
const isHoveringGallery = ref(false)
const zoomedScreenshot = ref<string | null>(null)
let galleryAutoTimer: ReturnType<typeof setInterval> | null = null
let catalogAutoRefreshTimer: ReturnType<typeof setInterval> | null = null
let runningPollTimer: ReturnType<typeof setInterval> | null = null

const previewImages = computed(() => {
	if (!selectedServer.value) return []
	return getServerPreviewImages(selectedServer.value)
})

const isPreviewFallback = computed(() => {
	return !hasCustomScreenshots(selectedServer.value)
})

const currentPreviewImage = computed(() => {
	const imgs = previewImages.value
	if (imgs.length === 0) return '/logo.png'
	const idx = Math.min(activeScreenshotIndex.value, imgs.length - 1)
	return imgs[idx]
})

const heroBackdropUrl = computed(() => {
	if (!selectedServer.value) return ''
	const imgs = previewImages.value
	return imgs[0] || resolveImageUrl(selectedServer.value.image || selectedServer.value.logo)
})

function getPagedDots(total: number, activeIndex: number, maxDots = 6): number[] {
	if (total <= 1) return []
	if (total <= maxDots) return Array.from({ length: total }, (_, idx) => idx)
	const half = Math.floor(maxDots / 2)
	let start = Math.max(0, activeIndex - half)
	if (start + maxDots > total) {
		start = Math.max(0, total - maxDots)
	}
	return Array.from({ length: maxDots }, (_, offset) => start + offset)
}

watch(selectedId, () => {
	activeScreenshotIndex.value = 0
	zoomedScreenshot.value = null
})

function findInstanceForServer(server?: Dr3shServer | null) {
	return findMatchingInstance(server, instances.value)
}

const matchingInstance = computed(() => findInstanceForServer(selectedServer.value))

const localInstalledVersion = computed(() => {
	// Touch reactive map so computed updates immediately after download or delete
	const _map = installedVersionsMap.value
	if (!selectedServer.value) return null
	return getInstalledVersion(selectedServer.value.id)
})

const isInstalled = computed(() => {
	return Boolean(matchingInstance.value && localInstalledVersion.value)
})

const hasUpdate = computed(() => {
	if (!isInstalled.value || !selectedVersion.value?.version || !localInstalledVersion.value) return false
	const cleanInstalled = localInstalledVersion.value.replace(/^v/i, '').trim()
	const cleanLatest = selectedVersion.value.version.replace(/^v/i, '').trim()
	return cleanInstalled !== cleanLatest
})

// Running state check
const isSelectedRunning = ref(false)

async function checkRunningState() {
	if (!matchingInstance.value?.id) {
		isSelectedRunning.value = false
		return
	}
	try {
		const procs = await get_by_instance_id(matchingInstance.value.id)
		isSelectedRunning.value = Array.isArray(procs) && procs.length > 0
	} catch {
		isSelectedRunning.value = false
	}
}

watch(matchingInstance, () => {
	void checkRunningState()
})

const buttonAction = computed<'running' | 'play' | 'update' | 'download' | 'maintenance'>(() => {
	if (maintenanceMode.value && !isDeveloper.value) return 'maintenance'
	if (isSelectedRunning.value) return 'running'
	if (!isInstalled.value) return 'download'
	if (hasUpdate.value) return 'update'
	return 'play'
})

const buttonLabel = computed(() => {
	if (buttonAction.value === 'maintenance') {
		return 'MANTENIMIENTO'
	}
	if (isDownloading.value && currentServerId.value === selectedServer.value?.id) {
		return 'DESCARGANDO...'
	}
	switch (buttonAction.value) {
		case 'running':
			return 'EN EJECUCIÓN'
		case 'play':
			return 'JUGAR'
		case 'update':
			return 'ACTUALIZAR'
		case 'download':
		default:
			return 'DESCARGAR'
	}
})

// Market hover preview state
const hoveredMarketId = ref<string | null>(null)
const marketHoverSlideIdx = ref(0)
let marketHoverTimer: ReturnType<typeof setInterval> | null = null

watch(hoveredMarketId, (id) => {
	marketHoverSlideIdx.value = 0
	if (marketHoverTimer) {
		clearInterval(marketHoverTimer)
		marketHoverTimer = null
	}
	if (id) {
		const srv = servers.value.find((s) => s.id === id)
		const imgs = getServerPreviewImages(srv)
		if (imgs.length > 1) {
			marketHoverTimer = setInterval(() => {
				marketHoverSlideIdx.value = (marketHoverSlideIdx.value + 1) % imgs.length
			}, 1600)
		}
	}
})

// =========================================================
// Custom Directive: Dynamic Scrolling Description (1:1 with Electron ScrollingDescription)
// =========================================================
function updateScrollDescMetrics(el: HTMLElement) {
	const inner = el.querySelector('.scrolling-desc-track') as HTMLElement | null
	if (!inner) return
	const diff = Math.ceil(inner.scrollHeight - el.clientHeight)
	if (diff > 3) {
		el.classList.add('is-overflowing')
		el.style.setProperty('--desc-scroll-distance', `-${diff + 4}px`)
		const durationSec = Math.max(7, Math.min(20, diff / 9 + 5)).toFixed(1)
		el.style.setProperty('--desc-scroll-duration', `${durationSec}s`)
	} else {
		el.classList.remove('is-overflowing')
		el.style.removeProperty('--desc-scroll-distance')
		el.style.removeProperty('--desc-scroll-duration')
	}
}

const vScrollDesc = {
	mounted(el: HTMLElement) {
		nextTick(() => updateScrollDescMetrics(el))
		const ro = new ResizeObserver(() => updateScrollDescMetrics(el))
		ro.observe(el)
		const inner = el.querySelector('.scrolling-desc-track')
		if (inner) ro.observe(inner)
		;(el as any).__scrollDescRo = ro
	},
	updated(el: HTMLElement) {
		nextTick(() => updateScrollDescMetrics(el))
	},
	unmounted(el: HTMLElement) {
		const ro = (el as any).__scrollDescRo as ResizeObserver | undefined
		if (ro) ro.disconnect()
	},
}

// =========================================================
// Settings State & Logic (1:1 with Electron Settings View, Max RAM = 80% of system total)
// =========================================================
const systemTotalRamMb = ref(16384)
const maxAllowedRamMb = computed(() => {
	return Math.max(2048, Math.floor((systemTotalRamMb.value * 0.8) / 512) * 512)
})
const settingsMemoryMb = ref(4096)
const settingsMinMemoryMb = ref(1024)
const settingsJavaAuto = ref(true)
const settingsJavaPath = ref('')
const settingsJvmArgs = ref('')
const settingsWidth = ref(1280)
const settingsHeight = ref(720)
const settingsFullscreen = ref(false)
const settingsCloseOnLaunchMode = ref<'keep' | 'minimize' | 'close'>(
	(localStorage.getItem('dr3sh.close_on_launch') as 'keep' | 'minimize' | 'close') || 'keep',
)
const settingsDiscordRpc = ref(true)
const settingsDefaultInstancesDir = ref<string>(localStorage.getItem('dr3sh.default_instances_dir') || '')
const detectedJavas = ref<Array<{ path: string; version: string }>>([])
const detectingJava = ref(false)
const rawBackendSettings = ref<any>(null)

// Accounts tab state (Native Microsoft Auth + Offline Accounts, no Skins section)
const accountModeTab = ref<'microsoft' | 'offline'>('offline')
const offlineUsernameInput = ref('')

const ramPresetOptions = computed(() => {
	return [2048, 4096, 6144, 8192, 10240, 12288, 16384, 24576, 32768].filter(
		(p) => p <= maxAllowedRamMb.value,
	)
})

const microsoftAccounts = computed(() => {
	return allAccounts.value.filter((a) => a.isMicrosoft)
})

const offlineAccounts = computed(() => {
	return allAccounts.value.filter((a) => !a.isMicrosoft)
})

watch(
	activeAccount,
	(acc) => {
		if (acc) {
			accountModeTab.value = acc.isMicrosoft ? 'microsoft' : 'offline'
			if (!acc.isMicrosoft && !offlineUsernameInput.value) {
				offlineUsernameInput.value = acc.name
			}
		}
	},
	{ immediate: true },
)

async function loadLauncherSettings() {
	try {
		const [s, maxMemKib, jres] = await Promise.all([
			getSettings().catch(() => null),
			get_max_memory().catch(() => null),
			get_java_versions().catch(() => null),
		])
		if (maxMemKib && typeof maxMemKib === 'number') {
			const mb = maxMemKib > 131072 ? Math.round(maxMemKib / 1024) : Math.round(maxMemKib)
			if (mb >= 2048) systemTotalRamMb.value = mb
		}
		if (jres && typeof jres === 'object') {
			const list = Object.entries(jres).map(([ver, info]: [string, any]) => ({
				version: info?.version || ver,
				path: info?.path || String(info),
			}))
			detectedJavas.value = list
		}
		if (s) {
			rawBackendSettings.value = s
			const savedMax = Number(s.memory?.maximum) || 4096
			settingsMemoryMb.value = Math.min(savedMax, maxAllowedRamMb.value)
			settingsMinMemoryMb.value = Math.min(
				Number(localStorage.getItem('dr3sh.min_ram_mb')) || 1024,
				settingsMemoryMb.value,
			)
			settingsJvmArgs.value = Array.isArray(s.extra_launch_args) ? s.extra_launch_args.join(' ') : ''
			settingsWidth.value = s.game_resolution?.[0] || 1280
			settingsHeight.value = s.game_resolution?.[1] || 720
			settingsFullscreen.value = Boolean(s.force_fullscreen)
			if (s.hide_on_process_start && settingsCloseOnLaunchMode.value === 'keep') {
				settingsCloseOnLaunchMode.value = 'close'
			}
			if (s.discord_rpc !== undefined && s.discord_rpc !== null) {
				settingsDiscordRpc.value = Boolean(s.discord_rpc)
			} else {
				settingsDiscordRpc.value = true
			}
		}
	} catch (e) {
		console.warn('Error loading settings:', e)
	}
}

async function saveLauncherSettings(notify = false) {
	try {
		const clampedRam = Math.min(Number(settingsMemoryMb.value) || 4096, maxAllowedRamMb.value)
		settingsMemoryMb.value = clampedRam
		if (settingsMinMemoryMb.value > clampedRam) {
			settingsMinMemoryMb.value = clampedRam
		}
		localStorage.setItem('dr3sh.min_ram_mb', String(settingsMinMemoryMb.value))
		localStorage.setItem('dr3sh.close_on_launch', settingsCloseOnLaunchMode.value)
		localStorage.setItem('dr3sh.default_instances_dir', settingsDefaultInstancesDir.value)

		const base = rawBackendSettings.value || (await getSettings())
		if (!base) return
		const extraArgs = settingsJvmArgs.value
			.split(/\s+/)
			.map((a) => a.trim())
			.filter(Boolean)
		const updated = {
			...base,
			memory: {
				...(base.memory || {}),
				maximum: clampedRam,
			},
			extra_launch_args: extraArgs,
			game_resolution: [Number(settingsWidth.value) || 1280, Number(settingsHeight.value) || 720],
			force_fullscreen: settingsFullscreen.value,
			hide_on_process_start: settingsCloseOnLaunchMode.value !== 'keep',
			discord_rpc: settingsDiscordRpc.value,
		}
		await setSettings(updated)
		rawBackendSettings.value = updated
		if (notify) {
			showToast('Ajustes guardados correctamente.')
		}
	} catch (e) {
		console.warn('Error saving settings:', e)
	}
}

function applyAikarFlags() {
	settingsJvmArgs.value = AIKAR_JVM_FLAGS
	void saveLauncherSettings()
	showToast('Flags de optimización Aikar G1GC aplicadas.')
}

async function handleSelectJavaExe() {
	try {
		const selected = await openDialog({
			multiple: false,
			directory: false,
			title: 'Seleccionar ejecutable de Java (javaw.exe / java.exe)',
			filters: [{ name: 'Java Executable', extensions: ['exe', '*'] }],
		})
		if (selected) {
			settingsJavaPath.value = String(selected)
			settingsJavaAuto.value = false
			showToast(`Ruta de Java seleccionada: ${selected}`)
		}
	} catch (e) {
		console.warn('Error choosing Java:', e)
	}
}

async function handleRedetectJavas() {
	try {
		detectingJava.value = true
		const jres = await get_java_versions()
		if (jres && typeof jres === 'object') {
			detectedJavas.value = Object.entries(jres).map(([ver, info]: [string, any]) => ({
				version: info?.version || ver,
				path: info?.path || String(info),
			}))
		}
		showToast('Instalaciones de Java actualizadas.')
	} catch (e) {
		console.warn('Error redetecting Java:', e)
	} finally {
		detectingJava.value = false
	}
}

async function handleSelectDefaultInstancesDir() {
	try {
		const selected = await openDialog({
			directory: true,
			multiple: false,
			title: 'Seleccionar carpeta base predeterminada para instancias',
		})
		if (selected) {
			settingsDefaultInstancesDir.value = String(selected)
			await saveLauncherSettings(true)
		}
	} catch (e) {
		console.warn('Error choosing default instances dir:', e)
	}
}

async function handleOpenStorageFolder(kind: 'instances' | 'shared' | 'data') {
	try {
		if (kind === 'instances' && settingsDefaultInstancesDir.value) {
			await openPath(settingsDefaultInstancesDir.value)
			return
		}
		if (matchingInstance.value) {
			const full = await get_full_path(matchingInstance.value.id)
			if (full) {
				const parentIdx = Math.max(full.lastIndexOf('\\'), full.lastIndexOf('/'))
				const baseInstances = parentIdx > 0 ? full.slice(0, parentIdx) : full
				if (kind === 'instances') {
					await openPath(baseInstances)
					return
				}
				const rootIdx = Math.max(baseInstances.lastIndexOf('\\'), baseInstances.lastIndexOf('/'))
				const appRoot = rootIdx > 0 ? baseInstances.slice(0, rootIdx) : baseInstances
				await openPath(appRoot)
				return
			}
		}
		await openInstanceFolder()
	} catch (e: any) {
		showToast(`No se pudo abrir la carpeta: ${e?.message || e}`)
	}
}

async function handleOfflineLoginSubmit() {
	const name = offlineUsernameInput.value.trim()
	if (!name) return
	try {
		busy.value = true
		await loginOfflineAccount(name)
		showToast(`Cuenta sin conexión activa: ${name}`)
	} catch (e: any) {
		showToast(`Error al iniciar sesión: ${e?.message || e}`)
	} finally {
		busy.value = false
	}
}

async function handleMicrosoftLogin() {
	try {
		busy.value = true
		showToast('Abriendo el sistema de autenticación Microsoft del launcher...')
		const acc = await loginMicrosoftAccount()
		if (acc) {
			accountModeTab.value = 'microsoft'
			showToast(`Conectado con Microsoft como ${acc.name}`)
		}
	} catch (e: any) {
		showToast(`Error de autenticación Microsoft: ${e?.message || e}`)
	} finally {
		busy.value = false
	}
}

function openDeveloperPanelFromAbout() {
	window.dispatchEvent(new KeyboardEvent('keydown', { key: 'F10', shiftKey: true }))
}

let testingHeartbeatTimer: ReturnType<typeof setInterval> | null = null

onMounted(async () => {
	void fetchVersionsData()
	void loadLauncherSettings()
	window.addEventListener('click', closeInstanceMenu)

	// Gallery auto-advance every 3.8s unless hovered or zoomed
	galleryAutoTimer = setInterval(() => {
		if (currentView.value === 'home' && !isHoveringGallery.value && !zoomedScreenshot.value) {
			const total = previewImages.value.length
			if (total > 1) {
				activeScreenshotIndex.value = (activeScreenshotIndex.value + 1) % total
			}
		}
	}, 3800)

	// Poll catalog every 15s when automatic refresh mode is enabled
	catalogAutoRefreshTimer = setInterval(() => {
		if (launcherRefreshMode.value === 'automatic' && !isDownloading.value) {
			void fetchVersionsData()
		}
	}, 15000)

	// Poll running process state every 3s
	runningPollTimer = setInterval(() => {
		void checkRunningState()
	}, 3000)

	// Check and execute testing session cleanup for regular players (1:1 with Electron)
	if (testingMode.value && !isDeveloper.value) {
		try {
			const raw = localStorage.getItem('dr3sh.testing_instances')
			if (raw) {
				const data = JSON.parse(raw)
				const now = Date.now()
				const lastTime = Number(data.lastClosedAt || data.lastHeartbeat || 0)
				const hoursLimit = Number(testingHours.value || 1)
				const maxDurationMs = hoursLimit * 60 * 60 * 1000
				if (lastTime > 0 && now - lastTime >= maxDurationMs) {
					const srvMap = data.instances || {}
					const serverIds = Object.keys(srvMap)
					if (serverIds.length > 0) {
						for (const srvId of serverIds) {
							removeInstalledVersion(srvId)
						}
						localStorage.removeItem('dr3sh.testing_instances')
						showToast(
							`[Modo Testing] La sesión de prueba de ${hoursLimit}h ha finalizado. Se han limpiado las instancias temporales.`,
							6000,
						)
						await instancesQuery.refetch()
					}
				}
			}
		} catch (e) {
			console.warn('[Testing Mode] Error en comprobación de limpieza:', e)
		}
	}

	testingHeartbeatTimer = setInterval(() => {
		if (testingMode.value && !isDeveloper.value) {
			try {
				const raw = localStorage.getItem('dr3sh.testing_instances')
				const data = raw ? JSON.parse(raw) : { instances: {} }
				data.lastHeartbeat = Date.now()
				localStorage.setItem('dr3sh.testing_instances', JSON.stringify(data))
			} catch {}
		}
	}, 30000)

	window.addEventListener('beforeunload', () => {
		if (testingMode.value && !isDeveloper.value) {
			try {
				const raw = localStorage.getItem('dr3sh.testing_instances')
				const data = raw ? JSON.parse(raw) : { instances: {} }
				data.lastClosedAt = Date.now()
				data.lastHeartbeat = Date.now()
				localStorage.setItem('dr3sh.testing_instances', JSON.stringify(data))
			} catch {}
		}
	})
})

onUnmounted(() => {
	window.removeEventListener('click', closeInstanceMenu)
	if (toastTimer) clearTimeout(toastTimer)
	if (galleryAutoTimer) clearInterval(galleryAutoTimer)
	if (catalogAutoRefreshTimer) clearInterval(catalogAutoRefreshTimer)
	if (runningPollTimer) clearInterval(runningPollTimer)
	if (testingHeartbeatTimer) clearInterval(testingHeartbeatTimer)
	if (marketHoverTimer) clearInterval(marketHoverTimer)
})

async function handlePlayOrInstall() {
	if (isDownloading.value) return

	if (maintenanceMode.value && !isDeveloper.value) {
		showToast(
			maintenanceMessage.value ||
				'El launcher se encuentra en modo mantenimiento y optimización. Volveremos pronto.',
			5000,
		)
		return
	}

	if (buttonAction.value === 'running') {
		return
	}

	if (buttonAction.value === 'play' && matchingInstance.value) {
		try {
			busy.value = true
			notice.value = null
			// Ensure account exists before launching
			if (!activeAccount.value) {
				await refreshAccount()
			}
			await run(matchingInstance.value.id)
			isSelectedRunning.value = true
			showToast(`Iniciando ${matchingInstance.value.name}...`)
		} catch (err: any) {
			console.error('Launch failed:', err)
			handleSevereError(err, { instanceId: matchingInstance.value.id })
			showToast(`Error al iniciar: ${err?.message || err}`)
		} finally {
			busy.value = false
		}
	} else if (buttonAction.value === 'update' && selectedServer.value && selectedVersion.value) {
		try {
			busy.value = true
			const srv = selectedServer.value
			const ver = selectedVersion.value
			const prevCustomFolder = getInstalledFolder(srv.id, srv.title)
			showToast(`Actualizando ${srv.title} a la versión ${ver.version}...`)
			await downloadAndInstallModpack({
				server: srv,
				version: ver,
				matchingInstanceId: matchingInstance.value?.id,
				customFolder: prevCustomFolder,
				onSuccess: async () => {
					if (testingMode.value && !isDeveloper.value) {
						try {
							const raw = localStorage.getItem('dr3sh.testing_instances')
							const data = raw ? JSON.parse(raw) : { instances: {} }
							data.instances = data.instances || {}
							data.instances[srv.id] = { installedAt: Date.now() }
							data.lastHeartbeat = Date.now()
							localStorage.setItem('dr3sh.testing_instances', JSON.stringify(data))
						} catch {}
					}
					await instancesQuery.refetch()
					showToast(`¡${srv.title} se ha actualizado correctamente!`)
				},
				onError: (err) => {
					showToast(`Error al actualizar: ${err?.message || err}`)
				},
			})
		} catch (e: any) {
			showToast(`Fallo en la actualización: ${e?.message || e}`)
		} finally {
			busy.value = false
		}
	} else if (selectedServer.value) {
		// Open Install Location Modal (AppData vs Custom drive)
		if (matchingInstance.value?.id) {
			try {
				const p = await get_full_path(matchingInstance.value.id)
				if (p) {
					const idx = p.lastIndexOf('\\')
					if (idx > 0) defaultInstancesBaseDir.value = p.slice(0, idx)
				}
			} catch {}
		}
		installLocationModal.value = {
			server: selectedServer.value,
			mode: 'default',
			customFolder: '',
		}
	}
}

async function handleKillRunningInstance() {
	if (!matchingInstance.value) return
	try {
		await killInstance(matchingInstance.value.id)
		isSelectedRunning.value = false
		showToast(`Se ha forzado el cierre de ${selectedServer.value?.title || 'Minecraft'}.`)
	} catch (e: any) {
		showToast(`Error al cerrar instancia: ${e?.message || e}`)
	}
}

async function handleCancelDownload() {
	await cancelDownload()
	showToast('Descarga cancelada.')
}

async function browseCustomFolder() {
	try {
		const selected = await openDialog({
			directory: true,
			multiple: false,
			title: `Seleccionar carpeta para ${installLocationModal.value?.server.title || 'el modpack'}`,
		})
		if (selected && installLocationModal.value) {
			installLocationModal.value.mode = 'custom'
			installLocationModal.value.customFolder = String(selected)
		}
	} catch (e) {
		console.warn('Error selecting custom folder:', e)
	}
}

async function confirmInstallLocation() {
	if (!installLocationModal.value) return

	if (maintenanceMode.value && !isDeveloper.value) {
		showToast(
			maintenanceMessage.value ||
				'El launcher se encuentra en modo mantenimiento y optimización. Volveremos pronto.',
			5000,
		)
		installLocationModal.value = null
		return
	}

	const { server, mode, customFolder } = installLocationModal.value
	installLocationModal.value = null

	try {
		busy.value = true
		showToast(`Iniciando descarga e instalación de ${server.title}...`)

		const ver =
			Array.isArray(server.versions) && server.versions.length > 0
				? server.versions[0]
				: versions.value.find((v) => v.server === server.id) || selectedVersion.value

		if (!ver) {
			throw new Error('No se encontró una versión activa para descargar.')
		}

		const existingInst = findInstanceForServer(server)

		await downloadAndInstallModpack({
			server,
			version: ver,
			matchingInstanceId: existingInst?.id,
			customFolder: mode === 'custom' ? customFolder : undefined,
			onSuccess: async () => {
				if (testingMode.value && !isDeveloper.value) {
					try {
						const raw = localStorage.getItem('dr3sh.testing_instances')
						const data = raw ? JSON.parse(raw) : { instances: {} }
						data.instances = data.instances || {}
						data.instances[server.id] = { installedAt: Date.now() }
						data.lastHeartbeat = Date.now()
						localStorage.setItem('dr3sh.testing_instances', JSON.stringify(data))
					} catch {}
				}
				await instancesQuery.refetch()
				showToast(`¡${server.title} se ha instalado correctamente y está listo para jugar!`)
			},
			onError: (err) => {
				showToast(`Error en la descarga: ${err?.message || err}`)
			},
		})
	} catch (err: any) {
		console.error('Failed to install modpack:', err)
		showToast(`Error al instalar: ${err?.message || err}`)
	} finally {
		busy.value = false
	}
}

async function openInstanceFolder() {
	const customPath = selectedServer.value
		? getInstalledFolder(selectedServer.value.id, selectedServer.value.title)
		: null
	if (customPath) {
		try {
			await openPath(customPath)
			return
		} catch {}
	}
	if (matchingInstance.value) {
		try {
			await showInstanceInFolder(matchingInstance.value.id)
		} catch (e: any) {
			console.error('Failed to open instance folder:', e)
			showToast(`No se pudo abrir la carpeta: ${e?.message || e}`)
		}
	} else {
		showToast('La instancia aún no está instalada localmente.')
	}
}

async function repairInstance() {
	if (selectedServer.value && selectedVersion.value) {
		try {
			busy.value = true
			showToast(`Verificando y reparando archivos de ${selectedServer.value.title}...`)
			if (matchingInstance.value) {
				await install_existing_instance(matchingInstance.value.id, true).catch(() => {})
			}
			await downloadAndInstallModpack({
				server: selectedServer.value,
				version: selectedVersion.value,
				matchingInstanceId: matchingInstance.value?.id,
				customFolder: getInstalledFolder(selectedServer.value.id, selectedServer.value.title),
				onSuccess: async () => {
					await instancesQuery.refetch()
					showToast('Verificación y reparación completada.')
				},
			})
		} catch (e: any) {
			console.error('Repair failed:', e)
			showToast(`Error al verificar: ${e?.message || e}`)
		} finally {
			busy.value = false
		}
	}
}

async function handleExecuteDeleteConfirm() {
	if (!deleteConfirmModal.value) return
	const { server, mode } = deleteConfirmModal.value
	closeDeleteConfirm()

	try {
		busy.value = true
		const savedCustomFolder = getInstalledFolder(server.id, server.title)
		const inst = findInstanceForServer(server)
		if (inst) {
			await remove(inst.id)
		}
		await invoke('dr3sh_cleanup_deleted_instance', {
			customFolder: savedCustomFolder || null,
		}).catch(() => {})
		removeInstalledVersion(server.id)
		if (mode === 'pack-remove') {
			removeOwned(server.id)
			showToast(`Modpack "${server.title}" eliminado del launcher.`)
		} else {
			showToast(`Instalación local de "${server.title}" eliminada correctamente.`)
		}
		await instancesQuery.refetch()
	} catch (e: any) {
		showToast(`Error al eliminar: ${e?.message || e}`)
	} finally {
		busy.value = false
	}
}

const filteredMarketServers = computed(() => {
	if (!searchQuery.value.trim()) return servers.value
	const q = searchQuery.value.toLowerCase().trim()
	return servers.value.filter(
		(s) =>
			s.title.toLowerCase().includes(q) ||
			(s.description || '').toLowerCase().includes(q),
	)
})
</script>

<template>
	<!-- Toast Notice -->
	<div v-if="notice" class="toast">
		<span class="toast-dot" />
		<span>{{ notice }}</span>
		<button type="button" aria-label="Cerrar aviso" @click="notice = null">✕</button>
	</div>

	<!-- =========================================================
	     VIEW 1: HOME (BIBLIOTECA DE INSTANCIAS)
	     ========================================================= -->
	<div v-if="currentView === 'home' && selectedServer" class="home-layout">
		<!-- Hero Banner -->
		<section class="modrinth-hero">
			<div
				class="modrinth-hero-backdrop"
				:style="{ backgroundImage: `url(&quot;${getStageImageUrl(heroBackdropUrl)}&quot;)` }"
			/>
			<div class="modrinth-hero-content">
				<div class="modrinth-hero-left">
					<div class="modrinth-pack-avatar">
						<img
							:src="resolveImageUrl(selectedServer.logo_square || selectedServer.logo || selectedServer.image)"
							:alt="selectedServer.title"
							@error="handleImageError($event)"
						/>
					</div>
					<div class="modrinth-pack-meta">
						<div class="modrinth-title-row">
							<h1>{{ selectedServer.title }}</h1>
							<span v-if="hasUpdate" class="status-pill">Actualización disponible</span>
						</div>
						<div
							v-if="selectedServer.description"
							v-scroll-desc="selectedServer.description"
							class="scrolling-desc-viewport modrinth-pack-desc"
						>
							<p class="scrolling-desc-track">{{ selectedServer.description }}</p>
						</div>
						<div class="modrinth-badges">
							<span class="meta-badge loader-badge">
								<img :src="loaderIconUrl(selectedServer.mod_loader)" alt="" class="loader-logo-img" />
								<span>{{ formatLoaderName(selectedServer.mod_loader) }}</span>
								<small v-if="selectedServer.mod_loader_version">{{ selectedServer.mod_loader_version }}</small>
							</span>
							<span v-if="selectedServer.minecraft_version" class="meta-badge">
								Minecraft {{ selectedServer.minecraft_version }}
							</span>
							<span v-if="selectedVersion?.version" class="meta-badge">
								v{{ selectedVersion.version.replace(/^v/i, '') }}
							</span>
							<span v-if="selectedVersion?.size_mb" class="meta-badge">
								{{ selectedVersion.size_mb }} MB
							</span>
						</div>
					</div>
				</div>

				<div class="modrinth-hero-actions">
					<div class="play-action-group">
						<button
							type="button"
							class="play-button"
							:class="[`action-${buttonAction}`, { 'is-running': buttonAction === 'running' }]"
							:disabled="busy || isDownloading || buttonAction === 'running'"
							@click="handlePlayOrInstall"
						>
							<template v-if="buttonAction === 'running'">
								<span class="running-dot-indicator" aria-hidden="true" />
							</template>
							<svg
								v-else-if="buttonAction === 'update'"
								viewBox="0 0 20 20"
								aria-hidden="true"
								class="play-btn-icon"
							>
								<path d="M10 15V5m0 0-4 4m4-4 4 4" />
							</svg>
							<svg
								v-else-if="buttonAction === 'download'"
								viewBox="0 0 20 20"
								aria-hidden="true"
								class="play-btn-icon"
							>
								<path d="M10 3.5v9m0 0-3.5-3.5M10 12.5 13.5 9M4 15.5h12" />
							</svg>
							<svg v-else viewBox="0 0 20 20" aria-hidden="true" class="play-btn-icon">
								<path d="M6 4.5 15.5 10 6 15.5V4.5Z" fill="currentColor" stroke="none" />
							</svg>
							<span>{{ buttonLabel }}</span>
						</button>

						<button
							v-if="buttonAction === 'running'"
							type="button"
							class="kill-instance-button"
							title="Forzar el cierre de Minecraft inmediatamente"
							@click="handleKillRunningInstance"
						>
							<svg viewBox="0 0 20 20" aria-hidden="true">
								<rect x="5" y="5" width="10" height="10" rx="2" fill="currentColor" stroke="none" />
							</svg>
							<span>Forzar cierre</span>
						</button>

						<!-- Three Dots (⋮) Dropdown Anchor -->
						<div class="instance-menu-anchor" @click.stop>
							<button
								type="button"
								class="instance-menu-trigger"
								:class="{ active: instanceMenuOpen }"
								title="Opciones de la instancia"
								@click="toggleInstanceMenu"
							>
								<svg viewBox="0 0 20 20" aria-hidden="true">
									<circle cx="10" cy="4.5" r="1.6" fill="currentColor" stroke="none" />
									<circle cx="10" cy="10" r="1.6" fill="currentColor" stroke="none" />
									<circle cx="10" cy="15.5" r="1.6" fill="currentColor" stroke="none" />
								</svg>
							</button>

							<!-- Dropdown Menu -->
							<div v-if="instanceMenuOpen" class="instance-dropdown-menu" role="menu">
								<div class="instance-menu-header">
									<span class="instance-menu-header-icon">
										<img
											:src="resolveImageUrl(selectedServer.logo_square || selectedServer.logo || selectedServer.image)"
											:alt="selectedServer.title"
										/>
									</span>
									<div class="instance-menu-header-copy">
										<strong>{{ selectedServer.title }}</strong>
										<small>
											{{ formatLoaderName(selectedServer.mod_loader) }} •
											{{ isInstalled ? 'Instalado' : 'Sin instalar' }}
										</small>
									</div>
								</div>
								<button
									type="button"
									class="instance-menu-item"
									:disabled="busy || isDownloading"
									@click="repairInstance(); closeInstanceMenu()"
								>
									<svg viewBox="0 0 20 20" aria-hidden="true">
										<path
											d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l2-2a5.5 5.5 0 0 1-7.4 7.4l-6.2 6.2a1.8 1.8 0 0 1-2.6-2.6l6.2-6.2a5.5 5.5 0 0 1 7.4-7.4l-2.4 1.6Z"
										/>
									</svg>
									<span>Verificar y reparar instalación</span>
								</button>

								<button
									type="button"
									class="instance-menu-item"
									@click="openInstanceFolder(); closeInstanceMenu()"
								>
									<svg viewBox="0 0 20 20" aria-hidden="true">
										<path
											d="M2.5 5.5A1.5 1.5 0 0 1 4 4h4.2l1.8 2H16a1.5 1.5 0 0 1 1.5 1.5v7A1.5 1.5 0 0 1 16 16H4a1.5 1.5 0 0 1-1.5-1.5v-9Z"
										/>
									</svg>
									<span>Ir a la carpeta de la instancia</span>
								</button>

								<div class="instance-menu-divider" />

								<button
									type="button"
									class="instance-menu-item danger"
									:disabled="!isInstalled || busy || isDownloading"
									@click="requestDeleteConfirm(selectedServer, 'local-only'); closeInstanceMenu()"
								>
									<svg viewBox="0 0 20 20" aria-hidden="true">
										<path
											d="M4 6h12M7.5 6V4.5A1.5 1.5 0 0 1 9 3h2a1.5 1.5 0 0 1 1.5 1.5V6m2 0v9.5a1.5 1.5 0 0 1-1.5 1.5H7a1.5 1.5 0 0 1-1.5-1.5V6h9Z"
										/>
									</svg>
									<span>Eliminar instalación local</span>
								</button>

								<button
									type="button"
									class="instance-menu-item danger"
									:disabled="busy || isDownloading"
									@click="requestDeleteConfirm(selectedServer, 'pack-remove'); closeInstanceMenu()"
								>
									<svg viewBox="0 0 20 20" aria-hidden="true">
										<path d="M3 5h14M8 5V3h4v2m-6 3 1 9h6l1-9M7 8h6" />
									</svg>
									<span>Quitar modpack del launcher</span>
								</button>
							</div>
						</div>
					</div>
				</div>
			</div>

			<!-- Clean, Minimal Liquid Glass Hero Task Progress Bar -->
			<div
				v-if="isDownloading && currentServerId === selectedServer.id"
				class="hero-task-bar liquid-glass-task-bar"
				aria-live="polite"
			>
				<div class="task-header-row">
					<div class="task-title-group">
						<span class="task-spinner-orb" aria-hidden="true" />
						<strong class="task-phase-label">{{ downloadProgress.phaseText }}</strong>
					</div>
					<div class="task-badges-group">
						<span class="task-percent-pill">{{ downloadProgress.percent }}%</span>
						<button type="button" class="task-cancel-btn" @click="handleCancelDownload">
							Cancelar
						</button>
					</div>
				</div>

				<div class="progress-track liquid-shimmer-track">
					<span :style="{ width: `${Math.max(3, downloadProgress.percent)}%` }" />
				</div>
			</div>
		</section>

		<!-- Bottom 2 Columns Grid: Galería & Notas de versión -->
		<div class="modrinth-body-grid">
			<!-- Galería -->
			<section class="gallery-card">
				<div class="card-header">
					<h2>Galería</h2>
					<span v-if="previewImages.length > 1" class="card-counter">
						{{ activeScreenshotIndex + 1 }} / {{ previewImages.length }}
					</span>
				</div>
				<div
					class="gallery-viewer"
					@mouseenter="isHoveringGallery = true"
					@mouseleave="isHoveringGallery = false"
				>
					<div
						class="gallery-stage"
						title="Haz clic para ampliar la captura"
						@click="zoomedScreenshot = getStageImageUrl(currentPreviewImage)"
					>
						<img
							:src="getStageImageUrl(currentPreviewImage)"
							:alt="`Captura de ${selectedServer.title}`"
							:class="{ 'preview-fallback': isPreviewFallback }"
							@error="handleImageError($event, currentPreviewImage)"
						/>
						<div class="gallery-zoom-badge" aria-hidden="true">
							<svg viewBox="0 0 20 20">
								<circle cx="8.5" cy="8.5" r="5" />
								<path d="m12.5 12.5 4 4M8.5 6v5M6 8.5h5" />
							</svg>
							<span>Ampliar</span>
						</div>
					</div>

					<!-- Carousel dots cleanly below the stage -->
					<div v-if="previewImages.length > 1" class="gallery-stage-dots" role="tablist">
						<button
							v-for="idx in getPagedDots(previewImages.length, activeScreenshotIndex, 6)"
							:key="idx"
							type="button"
							class="gallery-stage-dot"
							:class="{ active: idx === activeScreenshotIndex }"
							:aria-label="`Ver captura ${idx + 1}`"
							@click="activeScreenshotIndex = idx"
						/>
					</div>

					<!-- Small Thumbnail Gallery Strip with all screenshots -->
					<div v-if="previewImages.length > 1" class="gallery-thumbs">
						<button
							v-for="(shot, idx) in previewImages"
							:key="`${selectedServer.id}-thumb-${idx}`"
							type="button"
							class="gallery-thumb"
							:class="{ active: idx === activeScreenshotIndex }"
							:aria-label="`Miniatura ${idx + 1}`"
							@click="activeScreenshotIndex = idx"
						>
							<img
								:src="getThumbnailUrl(shot)"
								:alt="`Miniatura ${idx + 1}`"
								loading="lazy"
								decoding="async"
								@error="handleImageError($event, shot)"
							/>
						</button>
					</div>
				</div>
			</section>

			<!-- Notas de versión -->
			<aside class="changelog-card">
				<div class="card-header">
					<h2>Notas de versión</h2>
					<span v-if="selectedVersion?.published_at" class="card-date">
						{{ selectedVersion.published_at }}
					</span>
				</div>
				<template v-if="selectedVersion">
					<div class="changelog-version-tag">
						<strong>{{ selectedVersion.title }}</strong>
						<code v-if="selectedVersion.version">
							v{{ selectedVersion.version.replace(/^v/i, '') }}
						</code>
					</div>
					<p class="changelog-body">
						{{ selectedVersion.description || 'Sin notas adicionales para esta versión.' }}
					</p>
				</template>
				<div v-else class="empty-card-note">
					Aún no hay notas de versión publicadas para este modpack.
				</div>
			</aside>
		</div>
	</div>

	<!-- Welcome / Empty Library State (Exact from Electron) -->
	<div v-else-if="currentView === 'home' && !selectedServer" class="welcome-layout">
		<div class="welcome-hero">
			<div class="welcome-logo-badge">
				<img src="/logo.png" alt="Dr3sh Launcher" class="welcome-logo" />
			</div>
			<h1 class="welcome-title">Dr3sh Launcher</h1>
			<p class="welcome-subtitle">
				Tu biblioteca está vacía. Explora los modpacks oficiales de la comunidad y añádelos para empezar a jugar.
			</p>
			<div class="welcome-actions">
				<button type="button" class="welcome-cta-button" @click="currentView = 'market'">
					<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="welcome-btn-icon">
						<circle cx="9" cy="9" r="6" />
						<line x1="14" y1="14" x2="19" y2="19" />
					</svg>
					<span>EXPLORAR MODPACKS</span>
				</button>
			</div>
		</div>

		<div class="welcome-features-grid">
			<div class="welcome-feature-card">
				<div class="welcome-feature-icon">
					<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="#1bd96a" stroke-width="2">
						<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
						<polyline points="7 10 12 15 17 10" />
						<line x1="12" y1="15" x2="12" y2="3" />
					</svg>
				</div>
				<h3>Descarga Rápida</h3>
				<p>Instalación automatizada de modpacks, mods y optimizaciones en un solo clic.</p>
			</div>

			<div class="welcome-feature-card">
				<div class="welcome-feature-icon">
					<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="#1bd96a" stroke-width="2">
						<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
					</svg>
				</div>
				<h3>Instancias Aisladas</h3>
				<p>Cada modpack tiene sus propias carpetas, configuraciones y partidas protegidas.</p>
			</div>

			<div class="welcome-feature-card">
				<div class="welcome-feature-icon">
					<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="#1bd96a" stroke-width="2">
						<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
						<circle cx="12" cy="7" r="4" />
					</svg>
				</div>
				<h3>Cuentas Flexibles</h3>
				<p>Juega con tu cuenta oficial de Microsoft o crea perfiles locales sin conexión fácilmente.</p>
			</div>
		</div>
	</div>

	<!-- =========================================================
	     VIEW 2: MARKET (EXPLORAR MODPACKS)
	     ========================================================= -->
	<div v-else-if="currentView === 'market'" class="market-layout">
		<div class="page-heading">
			<h1>Explorar modpacks</h1>
			<label class="search-box">
				<svg viewBox="0 0 20 20" aria-hidden="true" class="search-icon">
					<circle cx="9" cy="9" r="5.5" />
					<path d="m13.2 13.2 3.8 3.8" />
				</svg>
				<input
					v-model="searchQuery"
					type="search"
					placeholder="Buscar modpacks..."
				/>
			</label>
		</div>

		<div class="market-grid">
			<article
				v-for="server in filteredMarketServers"
				:key="server.id"
				class="market-item"
				@mouseenter="hoveredMarketId = server.id"
				@mouseleave="hoveredMarketId = null"
			>
				<!-- Blurred Screenshot Background Banner -->
				<div class="market-image" aria-hidden="true">
					<img
						:src="
							getThumbnailUrl(
								getServerPreviewImages(server)[0] ||
									resolveImageUrl(server.image || server.logo || server.logo_square),
							)
						"
						:alt="server.title"
						class="market-blurred-slide"
						@error="
							handleImageError(
								$event,
								getServerPreviewImages(server)[0] ||
									resolveImageUrl(server.image || server.logo || server.logo_square),
							)
						"
					/>
					<div class="market-image-scrim" />
				</div>

				<!-- Hover Screenshot Preview on Market Card -->
				<div
					v-if="hoveredMarketId === server.id && getServerPreviewImages(server).length > 0"
					class="library-preview-slideshow market"
					aria-hidden="true"
				>
					<img
						:src="
							getThumbnailUrl(
								getServerPreviewImages(server)[
									marketHoverSlideIdx % getServerPreviewImages(server).length
								],
							)
						"
						alt=""
						class="library-preview-img"
						@error="
							handleImageError(
								$event,
								getServerPreviewImages(server)[
									marketHoverSlideIdx % getServerPreviewImages(server).length
								],
							)
						"
					/>
					<div class="library-preview-overlay" />
					<div v-if="getServerPreviewImages(server).length > 1" class="library-preview-dots">
						<span
							v-for="(_, idx) in getServerPreviewImages(server).slice(0, 5)"
							:key="idx"
							class="library-preview-dot"
							:class="{ active: idx === marketHoverSlideIdx % Math.min(5, getServerPreviewImages(server).length) }"
						/>
					</div>
				</div>

				<div class="market-info">
					<div class="market-info-head">
						<div class="market-mini-icon">
							<img
								:src="resolveImageUrl(server.logo_square || server.logo || server.image)"
								:alt="server.title"
							/>
						</div>
						<div>
							<h2>{{ server.title }}</h2>
							<small>Minecraft {{ server.minecraft_version || '1.21.1' }}</small>
						</div>
					</div>
					<div
						v-scroll-desc="server.description"
						class="scrolling-desc-viewport market-desc"
					>
						<p class="scrolling-desc-track">
							{{ server.description || 'Disponible para instalar en tu biblioteca.' }}
						</p>
					</div>
					<div class="market-bottom">
						<span class="market-loader">
							<img :src="loaderIconUrl(server.mod_loader)" alt="" class="loader-logo-img" />
							{{ formatLoaderName(server.mod_loader) }}
						</span>
						<div v-if="owned.has(server.id)" class="market-owned-actions">
							<button
								type="button"
								class="get-button"
								@click="selectedId = server.id; currentView = 'home'"
							>
								Abrir
							</button>
							<button
								type="button"
								class="owned-button"
								@click="requestDeleteConfirm(server, 'pack-remove')"
							>
								Quitar
							</button>
						</div>
						<button
							v-else
							type="button"
							class="get-button"
							@click="addOwned(server.id); showToast(`Modpack &quot;${server.title}&quot; añadido a tu biblioteca.`)"
						>
							Añadir
						</button>
					</div>
				</div>
			</article>
		</div>

		<div v-if="filteredMarketServers.length === 0" class="no-results">
			No se encontraron modpacks con ese nombre.
		</div>
	</div>

	<!-- =========================================================
	     VIEW 3: SETTINGS (AJUSTES DEL LAUNCHER - 5 TABS)
	     ========================================================= -->
	<div v-else-if="currentView === 'settings'" class="settings-layout">
		<div class="page-heading">
			<div>
				<h1>Ajustes del Launcher</h1>
			</div>
			<button type="button" class="save-settings" @click="saveLauncherSettings(true)">
				Guardar ajustes
			</button>
		</div>

		<div class="settings-nav-tabs" role="tablist" aria-label="Categorías de ajustes">
			<button
				type="button"
				role="tab"
				:aria-selected="activeSettingsTab === 'memory'"
				class="settings-nav-tab"
				:class="{ active: activeSettingsTab === 'memory' }"
				@click="activeSettingsTab = 'memory'"
			>
				<span class="tab-icon" aria-hidden="true">
					<svg viewBox="0 0 24 24">
						<rect x="4" y="4" width="16" height="16" rx="2" />
						<rect x="9" y="9" width="6" height="6" />
						<path d="M9 1v3M15 1v3M9 20v3M15 20v3M20 9h3M20 14h3M1 9h3M1 14h3" />
					</svg>
				</span>
				<span>Java y Memoria</span>
			</button>

			<button
				type="button"
				role="tab"
				:aria-selected="activeSettingsTab === 'account'"
				class="settings-nav-tab"
				:class="{ active: activeSettingsTab === 'account' }"
				@click="activeSettingsTab = 'account'"
			>
				<span class="tab-icon" aria-hidden="true">
					<svg viewBox="0 0 24 24">
						<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
						<circle cx="12" cy="7" r="4" />
					</svg>
				</span>
				<span>Cuentas</span>
			</button>

			<button
				type="button"
				role="tab"
				:aria-selected="activeSettingsTab === 'game'"
				class="settings-nav-tab"
				:class="{ active: activeSettingsTab === 'game' }"
				@click="activeSettingsTab = 'game'"
			>
				<span class="tab-icon" aria-hidden="true">
					<svg viewBox="0 0 24 24">
						<line x1="6" y1="12" x2="10" y2="12" />
						<line x1="8" y1="10" x2="8" y2="14" />
						<line x1="15" y1="13" x2="15.01" y2="13" />
						<line x1="18" y1="11" x2="18.01" y2="11" />
						<rect x="2" y="6" width="20" height="12" rx="2" />
					</svg>
				</span>
				<span>Juego y Ventana</span>
			</button>

			<button
				type="button"
				role="tab"
				:aria-selected="activeSettingsTab === 'storage'"
				class="settings-nav-tab"
				:class="{ active: activeSettingsTab === 'storage' }"
				@click="activeSettingsTab = 'storage'"
			>
				<span class="tab-icon" aria-hidden="true">
					<svg viewBox="0 0 24 24">
						<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
					</svg>
				</span>
				<span>Almacenamiento</span>
			</button>

			<button
				type="button"
				role="tab"
				:aria-selected="activeSettingsTab === 'about'"
				class="settings-nav-tab"
				:class="{ active: activeSettingsTab === 'about' }"
				@click="activeSettingsTab = 'about'"
			>
				<span class="tab-icon" aria-hidden="true">
					<svg viewBox="0 0 24 24">
						<circle cx="12" cy="12" r="10" />
						<path d="M12 16v-4M12 8h.01" />
					</svg>
				</span>
				<span>Acerca de</span>
			</button>
		</div>

		<!-- TAB 1: JAVA Y MEMORIA -->
		<div v-if="activeSettingsTab === 'memory'" class="settings-tab-panel">
			<div class="system-hardware-banner">
				<div class="hardware-icon-wrap" aria-hidden="true">
					<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
						<rect x="2" y="4" width="20" height="16" rx="2" />
						<path d="M6 8h.01M10 8h.01M14 8h.01M18 8h.01M6 12h.01M10 12h.01M14 12h.01M18 12h.01" />
					</svg>
				</div>
				<div class="hardware-info-col">
					<div class="hardware-title-row">
						<b>Memoria RAM física en tu equipo:</b>
						<span class="hardware-ram-badge">
							{{ (systemTotalRamMb / 1024).toFixed(1) }} GB ({{ systemTotalRamMb }} MB)
						</span>
					</div>
					<small>
						Límite máximo seguro para Minecraft (80% del sistema):
						<strong>{{ (maxAllowedRamMb / 1024).toFixed(1) }} GB ({{ maxAllowedRamMb }} MB)</strong>
					</small>
				</div>
			</div>

			<section class="settings-section">
				<div class="settings-title">
					<div>
						<b>Memoria RAM de Minecraft</b>
						<small>Asigna hasta el 80% de tu memoria física para garantizar estabilidad del sistema operativo</small>
					</div>
				</div>

				<div class="settings-field-group">
					<div class="settings-slider-head">
						<div>
							<span>Memoria RAM máxima (`-Xmx`)</span>
							<small v-if="selectedServer" style="display: block; color: var(--brand); margin-top: 2px">
								Modpack actual: {{ selectedServer.title }}
							</small>
						</div>
						<div class="ram-value-pill">
							<strong>{{ (Math.min(settingsMemoryMb, maxAllowedRamMb) / 1024).toFixed(1) }} GB</strong>
							<span>({{ Math.min(settingsMemoryMb, maxAllowedRamMb) }} MB)</span>
						</div>
					</div>
					<div class="ram-control">
						<input
							v-model.number="settingsMemoryMb"
							type="range"
							min="1024"
							:max="maxAllowedRamMb"
							step="512"
							@change="saveLauncherSettings()"
						/>
					</div>
					<div class="settings-inline-actions" style="margin-top: 8px">
						<button
							v-for="preset in ramPresetOptions"
							:key="preset"
							type="button"
							class="settings-chip-btn"
							:class="{ active: settingsMemoryMb === preset }"
							@click="settingsMemoryMb = preset; saveLauncherSettings()"
						>
							{{ preset / 1024 }} GB
						</button>
					</div>
				</div>

				<div class="settings-field-group" style="margin-top: 14px">
					<div class="settings-slider-head">
						<span>Memoria RAM inicial (`-Xms`)</span>
						<div class="ram-value-pill">
							<strong>
								{{ (Math.min(settingsMinMemoryMb, settingsMemoryMb, maxAllowedRamMb) / 1024).toFixed(1) }} GB
							</strong>
							<span>({{ Math.min(settingsMinMemoryMb, settingsMemoryMb, maxAllowedRamMb) }} MB)</span>
						</div>
					</div>
					<div class="ram-control">
						<input
							v-model.number="settingsMinMemoryMb"
							type="range"
							min="512"
							:max="Math.min(settingsMemoryMb, maxAllowedRamMb)"
							step="256"
							@change="saveLauncherSettings()"
						/>
					</div>
				</div>
			</section>

			<section class="settings-section">
				<div class="settings-title">
					<div>
						<b>Entorno de Ejecución Java</b>
						<small>Selecciona la versión de Java para Minecraft o usa la descarga oficial recomendada</small>
					</div>
				</div>

				<div class="settings-field-group">
					<!-- Option 1: Automatic / Mojang Runtime -->
					<div
						class="java-card"
						:class="{ active: settingsJavaAuto || !settingsJavaPath }"
						@click="settingsJavaAuto = true; settingsJavaPath = ''; saveLauncherSettings()"
					>
						<div class="java-card-radio">
							<span class="java-radio-dot" :class="{ checked: settingsJavaAuto || !settingsJavaPath }" />
						</div>
						<div class="java-card-info">
							<div class="java-card-top">
								<span class="java-card-title">Automático (Runtime Oficial de Mojang)</span>
								<span class="java-tag recommended">Recomendado (Por defecto)</span>
							</div>
							<p class="java-card-desc">
								El launcher descarga y gestiona de forma aislada la versión exacta de Java oficial requerida por cada modpack (Java 21, 17 u 8) sin interferir con tu sistema.
							</p>
						</div>
					</div>

					<div class="java-detected-header">
						<span>Instalaciones detectadas en tu equipo:</span>
						<button
							type="button"
							class="settings-chip-btn"
							:disabled="detectingJava"
							@click="handleRedetectJavas"
						>
							{{ detectingJava ? 'Buscando...' : 'Actualizar lista' }}
						</button>
					</div>

					<div v-if="detectedJavas.length > 0" class="detected-java-list">
						<div
							v-for="item in detectedJavas"
							:key="item.path"
							class="java-card"
							:class="{ active: !settingsJavaAuto && settingsJavaPath.toLowerCase() === item.path.toLowerCase() }"
							@click="settingsJavaAuto = false; settingsJavaPath = item.path; saveLauncherSettings()"
						>
							<div class="java-card-radio">
								<span
									class="java-radio-dot"
									:class="{ checked: !settingsJavaAuto && settingsJavaPath.toLowerCase() === item.path.toLowerCase() }"
								/>
							</div>
							<div class="java-card-info">
								<div class="java-card-top">
									<span class="java-card-title">Java {{ item.version }}</span>
									<span class="java-tag ver">Java {{ item.version }}</span>
								</div>
								<span class="java-card-path">{{ item.path }}</span>
							</div>
						</div>
					</div>

					<div v-else-if="!detectingJava" class="java-empty-notice">
						Se utilizará el runtime automático oficial gestionado por el launcher.
					</div>

					<div class="java-custom-actions">
						<button type="button" class="settings-chip-btn" @click="handleSelectJavaExe">
							Explorar archivo javaw.exe...
						</button>
						<div v-if="!settingsJavaAuto && settingsJavaPath" class="java-custom-badge">
							Ruta personalizada activa: <code>{{ settingsJavaPath }}</code>
							<button
								type="button"
								class="settings-link-btn"
								style="margin-left: 8px"
								@click="settingsJavaAuto = true; settingsJavaPath = ''; saveLauncherSettings()"
							>
								Restablecer a automático
							</button>
						</div>
					</div>
				</div>

				<div class="settings-field-group">
					<div class="settings-slider-head">
						<span>Argumentos JVM adicionales</span>
						<button type="button" class="settings-link-btn" @click="applyAikarFlags">
							Usar flags G1GC (Aikar)
						</button>
					</div>
					<input
						v-model="settingsJvmArgs"
						class="settings-input"
						placeholder="-XX:+UseG1GC -XX:+UnlockExperimentalVMOptions ..."
						@change="saveLauncherSettings()"
					/>
				</div>
			</section>
		</div>

		<!-- TAB 2: CUENTAS (SISTEMA NATIVO MICROSOFT + CUENTAS OFFLINE) -->
		<div v-else-if="activeSettingsTab === 'account'" class="settings-tab-panel">
			<section class="settings-section">
				<div class="settings-title">
					<div>
						<b>Cuentas de Minecraft</b>
						<small>Autenticación oficial de Microsoft integrada en el launcher y soporte para cuentas sin conexión (Offline)</small>
					</div>
				</div>

				<div class="account-mode-switch" role="group" aria-label="Tipo de cuenta">
					<button
						type="button"
						:class="{ active: accountModeTab === 'microsoft' }"
						@click="accountModeTab = 'microsoft'"
					>
						Cuenta Microsoft
					</button>
					<button
						type="button"
						:class="{ active: accountModeTab === 'offline' }"
						@click="accountModeTab = 'offline'"
					>
						Sin conexión (Offline)
					</button>
				</div>

				<template v-if="accountModeTab === 'microsoft'">
					<div class="microsoft-account-controls">
						<div class="account-list">
							<div
								v-for="acc in microsoftAccounts"
								:key="acc.id"
								class="account-row"
								:class="{ active: activeAccount?.id === acc.id }"
							>
								<button
									type="button"
									class="account-select"
									@click="switchActiveAccount(acc.id)"
								>
									<span class="account-avatar">
										<img
											:src="`https://mc-heads.net/avatar/${acc.uuid || encodeURIComponent(acc.name)}/64`"
											alt=""
											class="skin-face-img"
										/>
									</span>
									<span>
										<b>{{ acc.name }}</b>
										<small>
											{{
												activeAccount?.id === acc.id
													? 'Activa - Cuenta Oficial Microsoft'
													: 'Microsoft - Minecraft Java'
											}}
										</small>
									</span>
								</button>
								<button
									type="button"
									class="account-remove"
									:title="`Quitar ${acc.name}`"
									@click="removeSavedAccount(acc.id)"
								>
									<svg viewBox="0 0 20 20" aria-hidden="true">
										<path d="M5 5l10 10M15 5 5 15" />
									</svg>
								</button>
							</div>

							<div v-if="microsoftAccounts.length === 0" class="java-empty-notice">
								No hay ninguna cuenta Microsoft vinculada todavía. Pulsa el botón inferior para iniciar sesión con el sistema oficial del launcher.
							</div>
						</div>

						<button
							type="button"
							class="account-add"
							:disabled="busy"
							@click="handleMicrosoftLogin"
						>
							{{ busy ? 'Conectando con Microsoft...' : 'Iniciar sesión con Microsoft' }}
						</button>
					</div>
				</template>

				<template v-else>
					<form class="settings-field-group" @submit.prevent="handleOfflineLoginSubmit">
						<label class="settings-label">
							<span>Nombre de jugador (Offline)</span>
							<div class="settings-inline-actions" style="display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 10px">
								<input
									v-model="offlineUsernameInput"
									class="settings-input"
									maxlength="16"
									placeholder="Tu nombre en Minecraft (ej. fkdr3sh)"
								/>
								<button
									type="submit"
									class="account-add"
									:disabled="busy || !offlineUsernameInput.trim()"
								>
									Usar cuenta Offline
								</button>
							</div>
						</label>
					</form>

					<div v-if="offlineAccounts.length > 0" class="microsoft-account-controls" style="margin-top: 8px">
						<span class="settings-field-caption">Cuentas sin conexión guardadas:</span>
						<div class="account-list">
							<div
								v-for="acc in offlineAccounts"
								:key="acc.id"
								class="account-row"
								:class="{ active: activeAccount?.id === acc.id }"
							>
								<button
									type="button"
									class="account-select"
									@click="switchActiveAccount(acc.id)"
								>
									<span class="account-avatar">
										<img
											:src="`https://mc-heads.net/avatar/${encodeURIComponent(acc.name)}/64`"
											alt=""
											class="skin-face-img"
										/>
									</span>
									<span>
										<b>{{ acc.name }}</b>
										<small>
											{{ activeAccount?.id === acc.id ? 'Activa - Modo Sin Conexión' : 'Sin conexión (Offline)' }}
										</small>
									</span>
								</button>
								<button
									type="button"
									class="account-remove"
									:title="`Quitar ${acc.name}`"
									@click="removeSavedAccount(acc.id)"
								>
									<svg viewBox="0 0 20 20" aria-hidden="true">
										<path d="M5 5l10 10M15 5 5 15" />
									</svg>
								</button>
							</div>
						</div>
					</div>
				</template>
			</section>
		</div>

		<!-- TAB 3: JUEGO Y VENTANA -->
		<div v-else-if="activeSettingsTab === 'game'" class="settings-tab-panel">
			<section class="settings-section">
				<div class="settings-title">
					<div>
						<b>Ventana de Juego y Lanzamiento</b>
						<small>Resolución inicial de Minecraft y comportamiento al abrir el juego</small>
					</div>
				</div>

				<label class="settings-toggle-row">
					<div>
						<b>Iniciar en pantalla completa</b>
						<small>Abre Minecraft directamente en modo pantalla completa</small>
					</div>
					<input
						v-model="settingsFullscreen"
						type="checkbox"
						@change="saveLauncherSettings()"
					/>
				</label>

				<div class="settings-field-group">
					<span class="settings-field-caption">Resolución de ventana (Ancho × Alto)</span>
					<div class="resolution-inputs">
						<input
							v-model.number="settingsWidth"
							type="number"
							class="settings-input"
							min="640"
							max="7680"
							:disabled="settingsFullscreen"
							aria-label="Ancho de ventana"
							@change="saveLauncherSettings()"
						/>
						<span class="resolution-sep">×</span>
						<input
							v-model.number="settingsHeight"
							type="number"
							class="settings-input"
							min="480"
							max="4320"
							:disabled="settingsFullscreen"
							aria-label="Alto de ventana"
							@change="saveLauncherSettings()"
						/>
					</div>
					<div class="settings-inline-actions">
						<button
							v-for="preset in [
								{ label: '854×480', w: 854, h: 480 },
								{ label: '1280×720', w: 1280, h: 720 },
								{ label: '1600×900', w: 1600, h: 900 },
								{ label: '1920×1080', w: 1920, h: 1080 },
								{ label: '2560×1440', w: 2560, h: 1440 },
							]"
							:key="preset.label"
							type="button"
							:disabled="settingsFullscreen"
							class="settings-chip-btn"
							:class="{ active: settingsWidth === preset.w && settingsHeight === preset.h }"
							@click="settingsWidth = preset.w; settingsHeight = preset.h; saveLauncherSettings()"
						>
							{{ preset.label }}
						</button>
					</div>
				</div>

				<label class="settings-label">
					<span>Al iniciar Minecraft</span>
					<select
						v-model="settingsCloseOnLaunchMode"
						class="settings-input"
						@change="saveLauncherSettings()"
					>
						<option value="keep">Mantener el launcher abierto</option>
						<option value="minimize">Minimizar el launcher al iniciar el juego</option>
						<option value="close">Cerrar el launcher al iniciar el juego</option>
					</select>
				</label>

				<label class="settings-toggle-row">
					<div>
						<b>Mostrar actividad en Discord (Rich Presence)</b>
						<small>Muestra en tu perfil de Discord que estás jugando en Dr3sh Launcher</small>
					</div>
					<input
						v-model="settingsDiscordRpc"
						type="checkbox"
						@change="saveLauncherSettings()"
					/>
				</label>
			</section>
		</div>

		<!-- TAB 4: ALMACENAMIENTO -->
		<div v-else-if="activeSettingsTab === 'storage'" class="settings-tab-panel">
			<section class="settings-section">
				<div class="settings-title">
					<div>
						<b>Directorios del Launcher</b>
						<small>Gestión de ubicaciones de instancias en disco, Java y respaldos</small>
					</div>
				</div>

				<div class="settings-field-group" style="margin-bottom: 8px">
					<div class="settings-slider-head">
						<span class="settings-field-caption">Carpeta base predeterminada para modpacks</span>
						<button
							v-if="settingsDefaultInstancesDir"
							type="button"
							class="settings-link-btn"
							@click="settingsDefaultInstancesDir = ''; saveLauncherSettings(true)"
						>
							Restablecer a estándar
						</button>
					</div>
					<div class="settings-inline-actions" style="display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 10px; margin-top: 6px">
						<input
							class="settings-input"
							readonly
							:value="settingsDefaultInstancesDir || 'Por defecto: %APPDATA%\\Dr3shLauncher\\instances'"
							placeholder="Ruta estándar del sistema"
						/>
						<button
							type="button"
							class="settings-chip-btn active"
							style="height: 42px; padding: 0 18px"
							@click="handleSelectDefaultInstancesDir"
						>
							Examinar...
						</button>
					</div>
				</div>

				<div class="directory-actions-list">
					<button
						type="button"
						class="directory-action-btn"
						@click="handleOpenStorageFolder('instances')"
					>
						<div>
							<b>Carpeta de instancias</b>
							<small>Mundos, mods, resourcepacks y opciones de cada modpack</small>
						</div>
						<span>Abrir carpeta</span>
					</button>

					<button
						type="button"
						class="directory-action-btn"
						@click="handleOpenStorageFolder('shared')"
					>
						<div>
							<b>Carpeta de Java y assets compartidos</b>
							<small>Runtimes de Java descargados, librerías y recursos de Minecraft</small>
						</div>
						<span>Abrir carpeta</span>
					</button>

					<button
						type="button"
						class="directory-action-btn"
						@click="handleOpenStorageFolder('data')"
					>
						<div>
							<b>Carpeta de datos de Dr3sh Launcher</b>
							<small>Configuración general, cachés y respaldos del launcher</small>
						</div>
						<span>Abrir carpeta</span>
					</button>
				</div>
			</section>
		</div>

		<!-- TAB 5: ACERCA DE -->
		<div v-else-if="activeSettingsTab === 'about'" class="settings-tab-panel">
			<section class="settings-section">
				<div class="settings-title">
					<div>
						<b>Acerca de Dr3sh Launcher</b>
						<small>Información de la versión y canal de actualizaciones</small>
					</div>
				</div>

				<div class="about-launcher-card">
					<div class="about-launcher-row">
						<span>Versión actual instalada</span>
						<strong class="about-version-tag">v0.2.16</strong>
					</div>
					<div class="about-launcher-row">
						<span>Actualizaciones automáticas</span>
						<span class="about-channel-status">GitHub Releases (Activo)</span>
					</div>
					<div class="about-launcher-row">
						<span>Sincronización de catálogo</span>
						<button
							type="button"
							class="settings-chip-btn active"
							:disabled="busy"
							@click="fetchVersionsData(); showToast('Catálogo sincronizado con GitHub Releases.')"
						>
							Sincronizar catálogo ahora
						</button>
					</div>
					<div class="about-launcher-row">
						<span>Canal de actualización</span>
						<span class="about-launcher-val">Actualizaciones oficiales de GitHub</span>
					</div>
					<div class="about-launcher-row">
						<span>Administración de modpacks</span>
						<button
							type="button"
							class="settings-chip-btn"
							@click="openDeveloperPanelFromAbout"
						>
							Estudio de Desarrollador (Shift+F10)
						</button>
					</div>
				</div>
			</section>
		</div>
	</div>

	<!-- =========================================================
	     GLOBAL MODALS (INSTALL LOCATION, DELETE CONFIRM, ZOOM)
	     ========================================================= -->

	<!-- 1. Install Location Modal (AppData vs Custom Drive) -->
	<div
		v-if="installLocationModal"
		class="confirm-modal-backdrop"
		role="dialog"
		aria-modal="true"
		@click="installLocationModal = null"
	>
		<div class="install-location-modal-box" @click.stop>
			<div class="install-location-modal-header">
				<div class="install-modal-icon-wrap" aria-hidden="true">
					<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
						<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
						<polyline points="7 10 12 15 17 10" />
						<line x1="12" y1="15" x2="12" y2="3" />
					</svg>
				</div>
				<div>
					<h3>¿Dónde quieres instalar {{ installLocationModal.server.title }}?</h3>
					<p class="confirm-modal-target">
						Elige la carpeta de destino antes de descargar los archivos del modpack
					</p>
				</div>
			</div>

			<div class="install-location-options">
				<!-- Option 1: Default AppData -->
				<div
					class="install-option-card"
					:class="{ selected: installLocationModal.mode === 'default' }"
					@click="installLocationModal.mode = 'default'"
				>
					<div class="install-option-radio">
						<div class="radio-dot" :class="{ checked: installLocationModal.mode === 'default' }" />
					</div>
					<div class="install-option-content">
						<div class="install-option-title-row">
							<b>Ubicación predeterminada (AppData)</b>
							<span class="install-badge-default">Recomendado</span>
						</div>
						<p class="install-option-desc">
							Instalar dentro de la carpeta oficial de datos del launcher en tu disco principal.
						</p>
						<div class="install-option-path-preview">
							<span>Ruta:</span>
							<code>{{ defaultInstancesBaseDir }}\{{ installLocationModal.server.title }}</code>
						</div>
					</div>
				</div>

				<!-- Option 2: Custom Drive / Folder -->
				<div
					class="install-option-card"
					:class="{ selected: installLocationModal.mode === 'custom' }"
					@click="installLocationModal.mode = 'custom'"
				>
					<div class="install-option-radio">
						<div class="radio-dot" :class="{ checked: installLocationModal.mode === 'custom' }" />
					</div>
					<div class="install-option-content">
						<div class="install-option-title-row">
							<b>Otra unidad o carpeta personalizada</b>
							<span class="install-badge-custom">Disco externo / secundario</span>
						</div>
						<p class="install-option-desc">
							Elige cualquier disco o directorio de tu PC. Se creará automáticamente la subcarpeta
							<code>{{ installLocationModal.server.title }}</code> dentro.
						</p>
						<div class="install-custom-picker-row" @click.stop>
							<input
								v-model="installLocationModal.customFolder"
								type="text"
								readonly
								class="install-folder-input"
								placeholder="Ninguna carpeta seleccionada..."
							/>
							<button
								type="button"
								class="settings-secondary-btn"
								@click="browseCustomFolder"
							>
								Examinar...
							</button>
						</div>
						<div v-if="installLocationModal.customFolder" class="install-final-path-alert">
							<svg viewBox="0 0 20 20" fill="currentColor">
								<path
									fill-rule="evenodd"
									d="M10 18a8 8 0 100-16 8 8 0 000 16zm3.707-9.293a1 1 0 00-1.414-1.414L9 10.586 7.707 9.293a1 1 0 00-1.414 1.414l2 2a1 1 0 001.414 0l4-4z"
									clip-rule="evenodd"
								/>
							</svg>
							<span>
								Se instalará en:
								<strong>
									{{ installLocationModal.customFolder.replace(/[\\/]$/, '') }}\{{ installLocationModal.server.title }}
								</strong>
							</span>
						</div>
					</div>
				</div>
			</div>

			<div class="install-location-actions">
				<button
					type="button"
					class="confirm-modal-cancel-btn"
					@click="installLocationModal = null"
				>
					Cancelar
				</button>
				<button
					type="button"
					class="install-modal-confirm-btn"
					:disabled="installLocationModal.mode === 'custom' && !installLocationModal.customFolder.trim()"
					@click="confirmInstallLocation"
				>
					Instalar modpack
				</button>
			</div>
		</div>
	</div>

	<!-- 2. Delete / Remove Confirmation Modal -->
	<div
		v-if="deleteConfirmModal"
		class="confirm-modal-backdrop"
		role="dialog"
		aria-modal="true"
		@click="closeDeleteConfirm"
	>
		<div class="confirm-modal-box" @click.stop>
			<div class="confirm-modal-header">
				<div class="confirm-modal-icon-wrap" aria-hidden="true">
					<svg viewBox="0 0 24 24">
						<path
							d="M12 9v4m0 4h.01M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"
						/>
					</svg>
				</div>
				<div>
					<h3>
						{{
							deleteConfirmModal.mode === 'pack-remove'
								? '¿Quitar modpack del launcher?'
								: '¿Eliminar instalación local?'
						}}
					</h3>
					<p class="confirm-modal-target">{{ deleteConfirmModal.server.title }}</p>
				</div>
			</div>

			<div class="confirm-modal-body">
				<p v-if="deleteConfirmModal.mode === 'pack-remove'">
					Se borrarán <strong>todos los archivos y configuraciones locales</strong> de esta instancia y se
					quitará de tu biblioteca. Para volver a jugarlo deberás añadirlo e instalarlo nuevamente desde
					<strong>Explorar</strong>.
				</p>
				<p v-else>
					Se borrarán los archivos de la carpeta de la instancia en tu equipo para liberar espacio. El
					modpack seguirá en tu biblioteca con la opción de volver a <strong>Descargar</strong> cuando
					quieras.
				</p>
			</div>

			<div class="confirm-modal-actions">
				<button
					type="button"
					class="confirm-modal-cancel-btn"
					@click="closeDeleteConfirm"
				>
					Cancelar
				</button>
				<button
					type="button"
					class="confirm-modal-delete-btn"
					:disabled="deleteConfirmModal.secondsLeft > 0 || busy"
					@click="handleExecuteDeleteConfirm"
				>
					{{
						deleteConfirmModal.secondsLeft > 0
							? `Eliminar (${deleteConfirmModal.secondsLeft}s)`
							: 'Eliminar'
					}}
				</button>
			</div>
		</div>
	</div>

	<!-- 3. Zoomed Screenshot Modal with Prev/Next Navigation -->
	<div
		v-if="zoomedScreenshot"
		class="image-zoom-modal-backdrop"
		role="dialog"
		aria-modal="true"
		aria-label="Captura ampliada"
		@click="zoomedScreenshot = null"
	>
		<div class="image-zoom-modal-container" @click.stop>
			<div class="image-zoom-toolbar">
				<span class="image-zoom-title">
					{{ selectedServer?.title }} - {{ activeScreenshotIndex + 1 }} / {{ previewImages.length }}
				</span>
				<button
					type="button"
					class="image-zoom-close-btn"
					aria-label="Cerrar vista ampliada"
					@click="zoomedScreenshot = null"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M5 5l10 10M15 5 5 15" />
					</svg>
				</button>
			</div>

			<div class="image-zoom-content">
				<button
					v-if="previewImages.length > 1"
					type="button"
					class="image-zoom-nav-btn prev"
					aria-label="Captura anterior"
					@click.stop="
						activeScreenshotIndex = (activeScreenshotIndex - 1 + previewImages.length) % previewImages.length;
						zoomedScreenshot = previewImages[activeScreenshotIndex]
					"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M12.5 15l-5-5 5-5" />
					</svg>
				</button>

				<img
					:src="zoomedScreenshot"
					:alt="selectedServer?.title || 'Captura ampliada'"
					class="image-zoom-full"
				/>

				<button
					v-if="previewImages.length > 1"
					type="button"
					class="image-zoom-nav-btn next"
					aria-label="Siguiente captura"
					@click.stop="
						activeScreenshotIndex = (activeScreenshotIndex + 1) % previewImages.length;
						zoomedScreenshot = previewImages[activeScreenshotIndex]
					"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M7.5 5l5 5-5 5" />
					</svg>
				</button>
			</div>
		</div>
	</div>
</template>
