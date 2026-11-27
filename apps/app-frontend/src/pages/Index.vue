<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { getVersion } from '@tauri-apps/api/app'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { fetch as tauriFetch } from '@tauri-apps/plugin-http'
import { openPath, openUrl } from '@tauri-apps/plugin-opener'
import { useQuery } from '@tanstack/vue-query'
import { instanceListQueryOptions } from '@/pages/instance/query-options'
import {
	useDr3shLauncher,
	getAccentColor,
	getContrastTextColor,
	type Dr3shServer,
	getServerPreviewImages,
	getStageImageUrl,
	getThumbnailUrl,
	handleImageError,
	hasCustomScreenshots,
	loaderIconUrl,
	formatLoaderName,
	deleteConfirmModal,
	pendingInstallServer,
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
	hasServerOrRuntimeUpdate,
	getInstalledFolder,
	findMatchingInstance,
	removeInstalledVersion,
	reconcileOrphanedInstalledInstances,
	cancelActiveDownload,
	getDiagnosticsText,
	openLogsFolder,
	acknowledgeMigrationNotice,
	type Dr3shLastError,
} from '@/composables/use-dr3sh-downloader'
import { run, kill as killInstance, remove, get_full_path, edit as editInstance } from '@/helpers/instance'
import { install_existing_instance } from '@/helpers/install'
import { get as getSettings, set as setSettings } from '@/helpers/settings'
import { get_max_memory, get_java_versions } from '@/helpers/jre.js'
import { get_by_instance_id } from '@/helpers/process.js'
import {
	get_logs,
	get_output_by_filename,
	get_live_log_buffer,
	get_latest_log_cursor,
	clear_log_buffer,
} from '@/helpers/logs.js'
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
	isOffline,
	catalogError,
	retryingCatalog,
	fetchVersionsData,
	addOwned,
	removeOwned,
	resolveImageUrl,
	handleAvatarError,
	debugPreview,
	hasActiveGameRunning,
	parseReleaseDateMs,
	getRealNowMs,
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

async function handleRetryCatalog() {
	await fetchVersionsData(true)
	if (!catalogError.value && servers.value.length > 0) {
		showToast('Conexión restablecida. Catálogo actualizado.')
	} else {
		showToast('Sigue sin haber conexión. Comprueba tu internet e inténtalo de nuevo.')
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

watch(pendingInstallServer, (srv) => {
	if (!srv) return
	pendingInstallServer.value = null
	if (isDownloading.value || busy.value) return
	if (maintenanceMode.value && !isDeveloper.value) {
		showToast(
			maintenanceMessage.value ||
				'El launcher está en mantenimiento. Las instalaciones están pausadas temporalmente.',
		)
		return
	}
	installLocationModal.value = {
		server: srv,
		mode: 'default',
		customFolder: '',
	}
})

function handleIndexEscapeKeydown(e: KeyboardEvent) {
	if (e.key !== 'Escape') return
	if (zoomedScreenshot.value) {
		e.preventDefault()
		e.stopImmediatePropagation()
		zoomedScreenshot.value = null
		return
	}
	if (diagnosticsModalError.value) {
		e.preventDefault()
		e.stopImmediatePropagation()
		diagnosticsModalError.value = null
		return
	}
	if (logsModalOpen.value) {
		e.preventDefault()
		e.stopImmediatePropagation()
		closeInstanceLogsModal()
		return
	}
	if (installLocationModal.value) {
		e.preventDefault()
		e.stopImmediatePropagation()
		installLocationModal.value = null
		return
	}
	if (deleteConfirmModal.value) {
		e.preventDefault()
		e.stopImmediatePropagation()
		closeDeleteConfirm()
		return
	}
	if (instanceMenuOpen.value) {
		e.preventDefault()
		e.stopImmediatePropagation()
		closeInstanceMenu()
	}
}

onMounted(() => {
	window.addEventListener('keydown', handleIndexEscapeKeydown, true)
})

onUnmounted(() => {
	window.removeEventListener('keydown', handleIndexEscapeKeydown, true)
})

const {
	isDownloading,
	isCancelling,
	currentServerId,
	serverStatesMap,
	downloadProgress,
	downloadAndInstallModpack,
	repairModpack,
} = useDr3shDownloader()

const diagnosticsModalError = ref<Dr3shLastError | null>(null)
const diagnosticsDetailsOpen = ref(false)
const diagnosticsText = ref('')
const loadingDiagnostics = ref(false)

const hasPendingMigrationNotice = computed(() => {
	if (!selectedServer.value) return false
	return Boolean(serverStatesMap.value[selectedServer.value.id]?.migration_notice_pending)
})

async function handleDismissMigrationNotice() {
	if (!selectedServer.value) return
	try {
		await acknowledgeMigrationNotice(selectedServer.value.id)
	} catch {}
}

function openErrorDiagnosticsModal(err: any) {
	if (err?.code === 'E_CANCELLED') {
		showToast('Operación cancelada.')
		return
	}
	const code = err?.code || 'E_UNKNOWN'
	const opId = err?.op_id || err?.opId || 'unknown'
	const message = err?.message || String(err || 'Error inesperado durante la operación.')
	const details = err?.details || ''
	diagnosticsModalError.value = {
		code,
		op_id: opId,
		message,
		details,
	}
	diagnosticsDetailsOpen.value = false
	diagnosticsText.value = ''
}

async function handleViewErrorDetails() {
	if (diagnosticsDetailsOpen.value) {
		diagnosticsDetailsOpen.value = false
		return
	}
	loadingDiagnostics.value = true
	diagnosticsDetailsOpen.value = true
	try {
		diagnosticsText.value = await getDiagnosticsText(
			diagnosticsModalError.value?.op_id,
			selectedServer.value?.id,
		)
	} catch (e: any) {
		diagnosticsText.value = diagnosticsModalError.value?.details || String(e)
	} finally {
		loadingDiagnostics.value = false
	}
}

async function handleCopyDiagnostics() {
	try {
		const text =
			diagnosticsText.value ||
			(await getDiagnosticsText(diagnosticsModalError.value?.op_id, selectedServer.value?.id))
		diagnosticsText.value = text
		await navigator.clipboard.writeText(text)
		showToast('Diagnóstico copiado al portapapeles.')
	} catch (e: any) {
		showToast(`No se pudo copiar el diagnóstico: ${e?.message || e}`)
	}
}

async function handleOpenLogsFolder() {
	try {
		await openLogsFolder()
	} catch (e: any) {
		showToast(`No se pudo abrir la carpeta de logs: ${e?.message || e}`)
	}
}

// Gallery / Screenshot state
const activeScreenshotIndex = ref(0)
const isHoveringGallery = ref(false)
const zoomedScreenshot = ref<string | null>(null)
const isDocumentHidden = ref(typeof document !== 'undefined' ? document.hidden : false)
const zoomScale = ref(1)
const zoomPanX = ref(0)
const zoomPanY = ref(0)
const isDraggingZoom = ref(false)
let zoomDragStartX = 0
let zoomDragStartY = 0
let zoomOriginPanX = 0
let zoomOriginPanY = 0
const SCREENSHOT_COOLDOWN_MS = 450
let lastSlideSwitchAt = 0
let galleryAutoTimer: ReturnType<typeof setInterval> | null = null
let catalogAutoRefreshTimer: ReturnType<typeof setInterval> | null = null
let runningPollTimer: ReturnType<typeof setInterval> | null = null

const isPowerSaving = computed(() => isDocumentHidden.value || isSelectedRunning.value)

const slideBatchStart = computed(() => {
	const total = previewImages.value.length
	if (total <= 6) return 0
	return Math.max(0, Math.min(total - 6, activeScreenshotIndex.value - 2))
})

function resetZoomView() {
	zoomScale.value = 1
	zoomPanX.value = 0
	zoomPanY.value = 0
	isDraggingZoom.value = false
}

function openZoomedScreenshot(idx?: number) {
	const imgs = previewImages.value
	if (!imgs.length) return
	if (typeof idx === 'number' && idx >= 0 && idx < imgs.length) {
		activeScreenshotIndex.value = idx
	}
	const safeIdx = Math.min(activeScreenshotIndex.value, imgs.length - 1)
	resetZoomView()
	zoomedScreenshot.value = getStageImageUrl(imgs[safeIdx])
}

function closeZoomedScreenshot() {
	zoomedScreenshot.value = null
	resetZoomView()
}

function adjustZoom(delta: number) {
	const next = Math.min(4, Math.max(1, Math.round((zoomScale.value + delta) * 100) / 100))
	zoomScale.value = next
	if (next <= 1) {
		zoomPanX.value = 0
		zoomPanY.value = 0
	}
}

function handleZoomWheel(e: WheelEvent) {
	adjustZoom(e.deltaY < 0 ? 0.25 : -0.25)
}

function handleZoomMouseDown(e: MouseEvent) {
	if (zoomScale.value <= 1 || e.button !== 0) return
	isDraggingZoom.value = true
	zoomDragStartX = e.clientX
	zoomDragStartY = e.clientY
	zoomOriginPanX = zoomPanX.value
	zoomOriginPanY = zoomPanY.value
}

function handleZoomMouseMove(e: MouseEvent) {
	if (!isDraggingZoom.value) return
	zoomPanX.value = zoomOriginPanX + (e.clientX - zoomDragStartX)
	zoomPanY.value = zoomOriginPanY + (e.clientY - zoomDragStartY)
}

function handleZoomMouseUp() {
	isDraggingZoom.value = false
}

function navigateZoomedScreenshot(step: number) {
	const imgs = previewImages.value
	if (imgs.length <= 1) return
	const now = Date.now()
	if (now - lastSlideSwitchAt < SCREENSHOT_COOLDOWN_MS) return
	lastSlideSwitchAt = now
	activeScreenshotIndex.value = (activeScreenshotIndex.value + step + imgs.length) % imgs.length
	resetZoomView()
	zoomedScreenshot.value = getStageImageUrl(imgs[activeScreenshotIndex.value])
}

function restartGalleryTimer() {
	if (galleryAutoTimer) clearInterval(galleryAutoTimer)
	galleryAutoTimer = setInterval(() => {
		if (
			currentView.value === 'home' &&
			!isHoveringGallery.value &&
			!zoomedScreenshot.value &&
			!isPowerSaving.value
		) {
			const total = previewImages.value.length
			if (total > 1) {
				activeScreenshotIndex.value = (activeScreenshotIndex.value + 1) % total
			}
		}
	}, 3800)
}

function selectScreenshot(idx: number) {
	if (idx === activeScreenshotIndex.value) {
		openZoomedScreenshot(idx)
		return
	}
	const now = Date.now()
	if (now - lastSlideSwitchAt < SCREENSHOT_COOLDOWN_MS) return
	lastSlideSwitchAt = now
	activeScreenshotIndex.value = idx
	restartGalleryTimer()
}

function prevSlide() {
	const total = previewImages.value.length
	if (total <= 1) return
	const now = Date.now()
	if (now - lastSlideSwitchAt < SCREENSHOT_COOLDOWN_MS) return
	lastSlideSwitchAt = now
	activeScreenshotIndex.value = (activeScreenshotIndex.value - 1 + total) % total
	restartGalleryTimer()
}

function nextSlide() {
	const total = previewImages.value.length
	if (total <= 1) return
	const now = Date.now()
	if (now - lastSlideSwitchAt < SCREENSHOT_COOLDOWN_MS) return
	lastSlideSwitchAt = now
	activeScreenshotIndex.value = (activeScreenshotIndex.value + 1) % total
	restartGalleryTimer()
}

const isSearchExpanded = ref(false)
const marketSearchInput = ref<HTMLInputElement | null>(null)
function toggleSearchExpand() {
	isSearchExpanded.value = !isSearchExpanded.value
	if (isSearchExpanded.value) {
		nextTick(() => {
			marketSearchInput.value?.focus()
		})
	}
}

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
	const _map = installedVersionsMap.value
	if (!isInstalled.value) return false
	return hasServerOrRuntimeUpdate(selectedServer.value, selectedVersion.value, matchingInstance.value)
})

// Running & instance health state checks
const isSelectedRunning = ref(false)
const killingInstance = ref(false)
const selectedInstanceHealth = ref<{
	healthy: boolean
	needs_repair: boolean
	reason: string | null
}>({
	healthy: true,
	needs_repair: false,
	reason: null,
})

interface InstanceLogEntry {
	filename: string
	log_type: 'InfoLog' | 'CrashReport'
	age: number
	output?: string | null
}

const logsModalOpen = ref(false)
const logsModalInstanceId = ref<string | null>(null)
const logsModalInstanceName = ref<string>('')
const logsLoading = ref(false)
const logsEntries = ref<InstanceLogEntry[]>([])
const logSelectedSource = ref<string>('live')
const logRawText = ref('')
const logSearchQuery = ref('')
const logFilterMode = ref<'all' | 'errors' | 'crashes'>('all')
const logOutputEl = ref<HTMLElement | null>(null)
const logAutoScroll = ref(true)
const lastLaunchStartedAtSec = ref<Record<string, number>>({})
const seenCrashReportKeys = new Set<string>()
let logsPollTimer: ReturnType<typeof setInterval> | null = null
let liveLogCursor = 0
let lastRunningInstanceId: string | null = null

const crashReportEntries = computed(() =>
	logsEntries.value.filter((entry) => entry.log_type === 'CrashReport'),
)

const filteredSelectorEntries = computed(() => {
	if (logFilterMode.value === 'crashes') {
		return crashReportEntries.value
	}
	return logsEntries.value
})

const detectedCrashHint = computed<{ title: string; advice: string } | null>(() => {
	const text = logRawText.value
	if (!text) return null
	if (/OutOfMemoryError|Java heap space|GC overhead limit exceeded/i.test(text)) {
		return {
			title: 'Posible falta de memoria RAM (OutOfMemoryError)',
			advice:
				'Aumenta la memoria RAM asignada (-Xmx) en Ajustes → Java y Memoria (recomendado 4 GB – 8 GB según la memoria de tu PC).',
		}
	}
	if (
		/EXCEPTION_ACCESS_VIOLATION.*(?:atio6axx|nvoglv|ig[0-9]+icd|opengl)|GLFW error|Pixel format not accelerated|WGL: The driver does not appear to support OpenGL/i.test(
			text,
		)
	) {
		return {
			title: 'Error de drivers gráficos / OpenGL',
			advice:
				'Actualiza los drivers de tu tarjeta gráfica (NVIDIA / AMD / Intel) o comprueba si Windows está ejecutando Java con la GPU dedicada.',
		}
	}
	if (/UnsupportedClassVersionError|class file version \d+\.\d+/i.test(text)) {
		return {
			title: 'Versión de Java incompatible con el modpack',
			advice:
				'Cambia el entorno a "Automático (Runtime Oficial)" en Ajustes → Java y Memoria para usar la versión exacta que requiere este modpack.',
		}
	}
	if (
		/MixinApplyError|ModLoadingException|ModResolutionException|Incompatible mod set|Requires any version of|Could not execute entrypoint stage|Failed to create mod instance/i.test(
			text,
		)
	) {
		return {
			title: 'Error o incompatibilidad al cargar un mod',
			advice:
				'Un mod falló durante la carga o falta una dependencia. Si añadiste mods manuales, retíralos o ejecuta "Verificar y reparar instalación".',
		}
	}
	if (
		/---- Minecraft Crash Report ----|A fatal error has been detected by the Java Runtime Environment/i.test(
			text,
		)
	) {
		return {
			title: 'Se detectó un Crash Report de Minecraft',
			advice:
				'Usa el botón "Copiar log" de abajo y compártelo con soporte para identificar exactamente qué provocó el cierre.',
		}
	}
	return null
})

interface DisplayLogLine {
	text: string
	level: 'normal' | 'warn' | 'error'
}

const logStatsAndLines = computed<{
	lines: DisplayLogLine[]
	totalMatched: number
	truncated: boolean
}>(() => {
	const raw = logRawText.value
	if (!raw) {
		return { lines: [], totalMatched: 0, truncated: false }
	}
	const query = logSearchQuery.value.trim().toLowerCase()
	const onlyErrors = logFilterMode.value === 'errors'
	const rawLines = raw.split(/\r?\n/)
	const matched: DisplayLogLine[] = []
	for (const line of rawLines) {
		if (onlyErrors && !/\b(ERROR|FATAL|WARN|Exception|Caused by:|MixinApplyError|Crash Report)\b/i.test(line)) {
			continue
		}
		if (query && !line.toLowerCase().includes(query)) {
			continue
		}
		let level: 'normal' | 'warn' | 'error' = 'normal'
		if (/\b(ERROR|FATAL|Exception|Caused by:|MixinApplyError)\b/i.test(line)) {
			level = 'error'
		} else if (/\bWARN\b/i.test(line)) {
			level = 'warn'
		}
		matched.push({ text: line, level })
	}
	const MAX_RENDERED_LINES = 2500
	if (matched.length > MAX_RENDERED_LINES) {
		return {
			lines: matched.slice(-MAX_RENDERED_LINES),
			totalMatched: matched.length,
			truncated: true,
		}
	}
	return {
		lines: matched,
		totalMatched: matched.length,
		truncated: false,
	}
})

function scrollLogsToBottom() {
	if (!logAutoScroll.value) return
	void nextTick(() => {
		const el = logOutputEl.value
		if (el) {
			el.scrollTop = el.scrollHeight
		}
	})
}

function handleLogsScroll() {
	const el = logOutputEl.value
	if (!el) return
	logAutoScroll.value = el.scrollHeight - el.scrollTop - el.clientHeight < 48
}

function sortLogEntries(entries: InstanceLogEntry[]): InstanceLogEntry[] {
	return [...entries].sort((a, b) => {
		if (a.log_type !== b.log_type) {
			return a.log_type === 'CrashReport' ? -1 : 1
		}
		return (b.age || 0) - (a.age || 0) || b.filename.localeCompare(a.filename)
	})
}

async function refreshInstanceLogEntries(instanceId: string) {
	try {
		const list = (await get_logs(instanceId, true)) as InstanceLogEntry[]
		logsEntries.value = sortLogEntries(Array.isArray(list) ? list : [])
	} catch (e) {
		console.warn('Error loading log list:', e)
		logsEntries.value = []
	}
}

async function pollLiveLogsTick() {
	if (!logsModalOpen.value || logSelectedSource.value !== 'live' || !logsModalInstanceId.value) {
		return
	}
	const instId = logsModalInstanceId.value
	try {
		const res = await get_latest_log_cursor(instId, liveLogCursor)
		if (res) {
			const chunk = String(res.output || '')
			if (res.new_file) {
				liveLogCursor = Number(res.cursor || 0)
				logRawText.value = chunk
				scrollLogsToBottom()
				return
			}
			if (chunk) {
				liveLogCursor = Number(res.cursor || 0)
				logRawText.value = logRawText.value
					? `${logRawText.value}${logRawText.value.endsWith('\n') ? '' : '\n'}${chunk}`
					: chunk
				scrollLogsToBottom()
				return
			}
			liveLogCursor = Number(res.cursor || liveLogCursor)
		}
		if (!logRawText.value) {
			const liveBuf = await get_live_log_buffer(instId).catch(() => '')
			if (liveBuf) {
				logRawText.value = String(liveBuf)
				scrollLogsToBottom()
			}
		}
	} catch {}
}

function stopLogsPolling() {
	if (logsPollTimer) {
		clearInterval(logsPollTimer)
		logsPollTimer = null
	}
}

function startLogsPolling() {
	stopLogsPolling()
	logsPollTimer = setInterval(() => {
		void pollLiveLogsTick()
	}, 1200)
}

async function loadSelectedLogSource(sourceKey: string) {
	const instId = logsModalInstanceId.value
	if (!instId) return
	logSelectedSource.value = sourceKey
	logsLoading.value = true
	logAutoScroll.value = true
	try {
		if (sourceKey === 'live') {
			liveLogCursor = 0
			logRawText.value = ''
			const [cursorRes, liveBuf] = await Promise.all([
				get_latest_log_cursor(instId, 0).catch(() => null),
				get_live_log_buffer(instId).catch(() => ''),
			])
			if (cursorRes && typeof cursorRes.cursor === 'number') {
				liveLogCursor = cursorRes.cursor
			}
			logRawText.value = String(cursorRes?.output || liveBuf || '')
			startLogsPolling()
			scrollLogsToBottom()
			return
		}

		stopLogsPolling()
		const sepIdx = sourceKey.indexOf('::')
		const logType = sepIdx > 0 ? sourceKey.slice(0, sepIdx) : 'InfoLog'
		const filename = sepIdx > 0 ? sourceKey.slice(sepIdx + 2) : sourceKey
		const content = await get_output_by_filename(instId, logType, filename)
		logRawText.value = String(content || '')
		if (logType === 'CrashReport') {
			logAutoScroll.value = false
			void nextTick(() => {
				if (logOutputEl.value) logOutputEl.value.scrollTop = 0
			})
		} else {
			scrollLogsToBottom()
		}
	} catch (e: any) {
		logRawText.value = `No se pudo leer el log seleccionado: ${e?.message || e}`
	} finally {
		logsLoading.value = false
	}
}

function handleLogFilterModeChange(mode: 'all' | 'errors' | 'crashes') {
	logFilterMode.value = mode
	if (mode === 'crashes' && !logSelectedSource.value.startsWith('CrashReport::')) {
		const newestCrash = crashReportEntries.value[0]
		if (newestCrash) {
			void loadSelectedLogSource(`${newestCrash.log_type}::${newestCrash.filename}`)
		}
	}
}

async function openInstanceLogsModal(initialSource = 'live') {
	if (!matchingInstance.value?.id) {
		showToast('La instancia aún no está instalada localmente.')
		return
	}
	const instId = matchingInstance.value.id
	logsModalInstanceId.value = instId
	logsModalInstanceName.value = selectedServer.value?.title || matchingInstance.value.name
	logSearchQuery.value = ''
	logFilterMode.value = initialSource.startsWith('CrashReport::') ? 'crashes' : 'all'
	logsModalOpen.value = true
	await refreshInstanceLogEntries(instId)
	await loadSelectedLogSource(initialSource)
}

function closeInstanceLogsModal() {
	logsModalOpen.value = false
	stopLogsPolling()
}

async function handleCopyCurrentLog() {
	const hasActiveFilter = Boolean(logSearchQuery.value.trim()) || logFilterMode.value === 'errors'
	const textToCopy = hasActiveFilter
		? logStatsAndLines.value.lines.map((l) => l.text).join('\n')
		: logRawText.value
	if (!textToCopy) {
		showToast('No hay contenido de log para copiar.')
		return
	}
	try {
		await navigator.clipboard.writeText(textToCopy)
		showToast('Log copiado al portapapeles.')
	} catch (e: any) {
		showToast(`No se pudo copiar el log: ${e?.message || e}`)
	}
}

async function handleCopySupportReport() {
	try {
		const appVer = await getVersion().catch(() => '3.3.0')
		const srvName = selectedServer.value?.title || logsModalInstanceName.value || 'Sin seleccionar'
		const instVer = selectedInstalledVersion.value || 'Desconocida'
		const latestVer = selectedVersion.value?.version || 'Desconocida'
		const mcVer =
			selectedVersion.value?.minecraft_version || selectedServer.value?.minecraft_version || '?'
		const loader = formatLoaderName(
			selectedVersion.value?.mod_loader || selectedServer.value?.mod_loader,
		)
		const javaMode = settingsJavaAuto.value
			? 'Automático (Mojang Runtime)'
			: settingsJavaPath.value || 'Personalizado'
		const rawText = (logRawText.value || '').trim()
		const tailLines = rawText
			? rawText.split(/\r?\n/).slice(-60).join('\n')
			: '(Sin salida de log cargada)'
		const hintBlock = detectedCrashHint.value
			? `\n**Posible causa detectada:** ${detectedCrashHint.value.title} — ${detectedCrashHint.value.advice}`
			: ''

		const report = [
			`### Reporte de Soporte — Dr3sh App v${appVer}`,
			`**Modpack:** ${srvName} (Instalada: ${instVer} | Última: ${latestVer})`,
			`**Entorno:** Minecraft ${mcVer} • ${loader}`,
			`**RAM:** ${settingsMemoryMb.value} MB asignados / ${systemTotalRamMb.value} MB totales (Xms: ${settingsMinMemoryMb.value} MB)`,
			`**Java:** ${javaMode}`,
			`**Fuente de log:** ${logSelectedSource.value}${hintBlock}`,
			'',
			'```log',
			tailLines,
			'```',
		].join('\n')

		await navigator.clipboard.writeText(report)
		showToast('Reporte de soporte copiado (listo para Discord).')
	} catch (e: any) {
		showToast(`No se pudo copiar el reporte: ${e?.message || e}`)
	}
}

const DR3SH_SUPPORT_REPORT_URL = 'https://dr3sh-app-reports.dreshtext.workers.dev/'
const sendingSupportReport = ref(false)

async function handleSendSupportReport(
	kind: 'manual' | 'crash' | 'download_error' = 'manual',
	overrideError?: Dr3shLastError | null,
	overrideLogText?: string,
) {
	if (sendingSupportReport.value) return
	sendingSupportReport.value = true
	try {
		const appVersion = await getVersion().catch(() => '3.3.0')
		const server = selectedServer.value?.title || logsModalInstanceName.value || 'Sin seleccionar'
		const installedVersion = selectedInstalledVersion.value || 'No instalado'
		const latestVersion = selectedVersion.value?.version || '?'
		const mcVersion =
			selectedVersion.value?.minecraft_version || selectedServer.value?.minecraft_version || '?'
		const loader = formatLoaderName(
			selectedVersion.value?.mod_loader || selectedServer.value?.mod_loader,
		)
		const java = settingsJavaAuto.value
			? 'Automático (Mojang Runtime)'
			: settingsJavaPath.value || 'Personalizado'
		const ram = `${settingsMemoryMb.value} MB / ${systemTotalRamMb.value} MB`
		const username = activeAccount.value?.name || 'Anónimo'

		let logText = typeof overrideLogText === 'string' ? overrideLogText : logRawText.value || ''
		if (!logText.trim() && overrideError) {
			logText =
				(await getDiagnosticsText(overrideError.op_id, selectedServer.value?.id).catch(() => '')) ||
				overrideError.details ||
				''
		}

		const summary =
			overrideError?.message ||
			(detectedCrashHint.value
				? `${detectedCrashHint.value.title}: ${detectedCrashHint.value.advice}`
				: 'Reporte manual enviado desde Dr3sh App')

		const res = await tauriFetch(DR3SH_SUPPORT_REPORT_URL, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({
				appVersion,
				server,
				installedVersion,
				latestVersion,
				mcVersion,
				loader,
				ram,
				java,
				username,
				kind,
				errorCode: overrideError?.code || '',
				summary,
				logText: logText.slice(-150_000),
			}),
		})

		const data: any = await res.json().catch(() => null)
		if (!res.ok || !data?.ok) {
			throw new Error(data?.error || `HTTP ${res.status}`)
		}
		showToast('Reporte enviado al equipo de soporte. ¡Gracias!')
	} catch (e: any) {
		showToast(`No se pudo enviar el reporte: ${e?.message || e}`)
	} finally {
		sendingSupportReport.value = false
	}
}

async function handleClearLiveConsole() {
	if (logSelectedSource.value !== 'live' || !logsModalInstanceId.value) return
	logRawText.value = ''
	await clear_log_buffer(logsModalInstanceId.value).catch(() => {})
	showToast('Vista de consola limpiada.')
}

async function handleOpenCurrentLogsFolder() {
	const instId = logsModalInstanceId.value || matchingInstance.value?.id
	if (instId) {
		const instPath = await get_full_path(instId).catch(() => null)
		if (instPath) {
			const sep = instPath.includes('\\') ? '\\' : '/'
			const sub = logSelectedSource.value.startsWith('CrashReport::') ? 'crash-reports' : 'logs'
			try {
				await openPath(`${instPath}${sep}${sub}`)
				return
			} catch {
				try {
					await openPath(instPath)
					return
				} catch {}
			}
		}
	}
	await handleOpenLogsFolder()
}

async function checkForRecentCrashAfterStop(instanceId: string) {
	try {
		const entries = sortLogEntries(
			((await get_logs(instanceId, true)) as InstanceLogEntry[]) || [],
		)
		const crashes = entries.filter((e) => e.log_type === 'CrashReport')
		if (crashes.length === 0) return
		const nowSec = Math.floor(Date.now() / 1000)
		const launchStarted = lastLaunchStartedAtSec.value[instanceId] || 0
		const recentCrash = crashes.find((c) => {
			const key = `${instanceId}::${c.filename}`
			if (seenCrashReportKeys.has(key)) return false
			const isFreshByWallClock = nowSec - (c.age || 0) <= 120
			const isFromLatestLaunch = launchStarted > 0 && (c.age || 0) >= launchStarted - 5
			return isFreshByWallClock || isFromLatestLaunch
		})
		if (!recentCrash) return
		seenCrashReportKeys.add(`${instanceId}::${recentCrash.filename}`)
		showToast('Minecraft se cerró con un crash report. Abriendo diagnóstico...', 6000)
		if (matchingInstance.value?.id === instanceId) {
			await openInstanceLogsModal(`${recentCrash.log_type}::${recentCrash.filename}`)
		}
	} catch {}
}

async function checkRunningState() {
	const instId = matchingInstance.value?.id || null
	if (!instId) {
		isSelectedRunning.value = false
		hasActiveGameRunning.value = false
		lastRunningInstanceId = null
		return
	}
	try {
		const procs = await get_by_instance_id(instId)
		const nowRunning = Array.isArray(procs) && procs.length > 0
		const sameInstanceStopped =
			lastRunningInstanceId === instId && isSelectedRunning.value && !nowRunning
		isSelectedRunning.value = nowRunning
		hasActiveGameRunning.value = nowRunning
		lastRunningInstanceId = instId
		if (sameInstanceStopped) {
			void checkForRecentCrashAfterStop(instId)
		}
	} catch {
		isSelectedRunning.value = false
		hasActiveGameRunning.value = false
		lastRunningInstanceId = instId
	}
}

async function checkSelectedInstanceHealth() {
	const srv = selectedServer.value
	if (!srv) {
		selectedInstanceHealth.value = { healthy: true, needs_repair: false, reason: null }
		return
	}
	const expectsInstalled = Boolean(localInstalledVersion.value || matchingInstance.value)
	if (!expectsInstalled) {
		selectedInstanceHealth.value = { healthy: true, needs_repair: false, reason: null }
		return
	}
	try {
		let fallbackPath = getInstalledFolder(srv.id)
		if (!fallbackPath && matchingInstance.value?.id) {
			fallbackPath = await get_full_path(matchingInstance.value.id).catch(() => null)
		}
		const res = await invoke<{
			healthy: boolean
			needs_repair: boolean
			reason: string | null
		}>('dr3sh_check_instance_health', {
			serverId: srv.id,
			fallbackInstancePath: fallbackPath || null,
		})
		if (selectedServer.value?.id === srv.id) {
			selectedInstanceHealth.value = {
				healthy: Boolean(res?.healthy),
				needs_repair: Boolean(res?.needs_repair),
				reason: res?.reason ?? null,
			}
		}
	} catch {
		if (selectedServer.value?.id === srv.id) {
			selectedInstanceHealth.value = { healthy: true, needs_repair: false, reason: null }
		}
	}
}

watch(matchingInstance, () => {
	void checkRunningState()
})

watch(
	[selectedId, matchingInstance, localInstalledVersion, isDownloading],
	([_id, _inst, _ver, downloading]) => {
		if (downloading) return
		void checkSelectedInstanceHealth()
	},
	{ immediate: true },
)

// Live countdown clock & Preload state
interface PreloadStatusDto {
	preloaded: boolean
	version?: string | null
	release_date?: string | null
	locked_mods_count: number
}

const preloadStatus = ref<PreloadStatusDto>({
	preloaded: false,
	version: null,
	release_date: null,
	locked_mods_count: 0,
})

async function refreshPreloadStatus(serverId?: string) {
	if (!serverId) {
		preloadStatus.value = { preloaded: false, version: null, release_date: null, locked_mods_count: 0 }
		return
	}
	try {
		preloadStatus.value = await invoke<PreloadStatusDto>('dr3sh_get_preload_state', { serverId })
	} catch {
		preloadStatus.value = { preloaded: false, version: null, release_date: null, locked_mods_count: 0 }
	}
}

watch(
	() => selectedServer.value?.id,
	(sid) => {
		void refreshPreloadStatus(sid)
	},
	{ immediate: true },
)

watch(
	() => [selectedServer.value?.id, selectedServer.value?.logo_square, matchingInstance.value?.id],
	([sid, logoSq, instId]) => {
		if (!sid || !instId || !selectedServer.value) return
		const rawIcon = logoSq || selectedServer.value.logo || selectedServer.value.image
		if (!rawIcon) return
		void invoke('dr3sh_sync_desktop_shortcut_icon', {
			instanceName: selectedServer.value.title,
			instanceId: String(instId),
			serverId: String(sid),
			iconUrl: resolveImageUrl(String(rawIcon)),
		}).catch(() => {})
	},
	{ immediate: true },
)

const countdownNow = ref(getRealNowMs())
let countdownInterval: ReturnType<typeof setInterval> | null = null

onMounted(() => {
	countdownInterval = setInterval(() => {
		if (!isPowerSaving.value && (selectedServer.value?.release_date || isDeveloper.value)) {
			countdownNow.value = getRealNowMs()
		}
	}, 1000)
})

onUnmounted(() => {
	if (countdownInterval) {
		clearInterval(countdownInterval)
		countdownInterval = null
	}
})

const titleLogoFailed = ref(false)
const titleLogoLoaded = ref(false)
const titleLogoImgRef = ref<HTMLImageElement | null>(null)
watch(
	() => [selectedServer.value?.id, selectedServer.value?.title_logo],
	async () => {
		titleLogoFailed.value = false
		titleLogoLoaded.value = false
		await nextTick()
		if (titleLogoImgRef.value?.complete && titleLogoImgRef.value.naturalWidth > 0) {
			titleLogoLoaded.value = true
		}
	},
	{ immediate: true },
)

const isCountdownActive = computed(() => {
	if (!selectedServer.value?.release_date) return false
	const target = parseReleaseDateMs(selectedServer.value.release_date)
	return !isNaN(target) && target > countdownNow.value
})

const isPreloadWindowOpen = computed(() => {
	const srv = selectedServer.value
	if (!srv?.release_date || !srv.preload_hours_before || srv.preload_hours_before <= 0) return false
	const target = parseReleaseDateMs(srv.release_date)
	if (isNaN(target) || target <= countdownNow.value) return false
	const windowStart = target - srv.preload_hours_before * 3600_000
	return countdownNow.value >= windowStart
})

const countdownShortLabel = computed(() => {
	const rawDate = selectedServer.value?.release_date || preloadStatus.value?.release_date
	if (!rawDate) return ''
	const target = parseReleaseDateMs(rawDate)
	if (isNaN(target) || target <= countdownNow.value) return ''
	const diffSec = Math.floor((target - countdownNow.value) / 1000)
	const days = Math.floor(diffSec / 86400)
	const hours = Math.floor((diffSec % 86400) / 3600)
	const mins = Math.floor((diffSec % 3600) / 60)
	const secs = diffSec % 60
	const pad = (n: number) => String(n).padStart(2, '0')
	if (days > 0) {
		return `${days}d ${pad(hours)}h ${pad(mins)}m ${pad(secs)}s`
	}
	return `${pad(hours)}:${pad(mins)}:${pad(secs)}`
})

const countdownLabel = computed(() => {
	if (!countdownShortLabel.value) return ''
	return `Inicio del server en ${countdownShortLabel.value}`
})

const buttonAction = computed<
	'running' | 'repair' | 'play' | 'update' | 'download' | 'maintenance' | 'disabled' | 'countdown' | 'preload' | 'preloaded' | 'activate'
>(() => {
	if (isDeveloper.value && debugPreview.value.forceButtonAction !== 'auto') {
		return debugPreview.value.forceButtonAction
	}
	if (maintenanceMode.value && !isDeveloper.value) return 'maintenance'
	if (selectedServer.value?.disabled && !isDeveloper.value) return 'disabled'
	if (isCountdownActive.value && !isDeveloper.value) {
		if (preloadStatus.value.preloaded) return 'preloaded'
		if (isPreloadWindowOpen.value && !isInstalled.value) return 'preload'
		return 'countdown'
	}
	if (preloadStatus.value.preloaded && !isInstalled.value) {
		return 'play'
	}
	if (isSelectedRunning.value) return 'running'
	if (selectedInstanceHealth.value.needs_repair) return 'repair'
	if (!isInstalled.value) return 'download'
	if (hasUpdate.value) return 'update'
	return 'play'
})

const buttonLabel = computed(() => {
	if (buttonAction.value === 'maintenance') {
		return 'MANTENIMIENTO'
	}
	if (buttonAction.value === 'disabled') {
		return selectedServer.value?.disabled_message?.trim() || 'Modpack no disponible'
	}
	if (buttonAction.value === 'preload') {
		return 'Precargar Modpack'
	}
	if (buttonAction.value === 'preloaded') {
		return countdownShortLabel.value
			? `Precargado · Empieza en ${countdownShortLabel.value}`
			: 'Precargado · Esperando inicio del server'
	}
	if (buttonAction.value === 'activate') {
		return 'Activar y Jugar'
	}
	if (buttonAction.value === 'countdown') {
		return countdownLabel.value || 'Próximamente'
	}
	if (isDownloading.value && currentServerId.value === selectedServer.value?.id) {
		switch (downloadProgress.value.phase) {
			case 'preparing':
				return 'PREPARANDO...'
			case 'downloading':
				return (downloadProgress.value.attempt ?? 0) > 0
					? 'RECONECTANDO...'
					: 'DESCARGANDO...'
			case 'verifying':
				return 'VERIFICANDO...'
			case 'extracting':
				return 'DESCOMPRIMIENDO...'
			case 'applying':
			case 'finishing':
				return 'INSTALANDO...'
			default:
				return 'DESCARGANDO...'
		}
	}
	switch (buttonAction.value) {
		case 'running':
			return 'EN EJECUCIÓN'
		case 'repair':
			return 'REPARAR INSTANCIA'
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
	const p = inner.querySelector('p')
	const contentHeight = Math.ceil(p ? p.scrollHeight : inner.scrollHeight)
	const boxHeight = el.clientHeight || 42
	const diff = Math.ceil(contentHeight - boxHeight)
	if (diff > 4) {
		el.classList.add('is-overflowing')
		el.style.setProperty('--desc-scroll-distance', `-${diff + 6}px`)
		const durationSec = Math.max(6, Math.min(18, diff / 8 + 4)).toFixed(1)
		el.style.setProperty('--desc-scroll-duration', `${durationSec}s`)
	} else {
		el.classList.remove('is-overflowing')
		el.style.removeProperty('--desc-scroll-distance')
		el.style.removeProperty('--desc-scroll-duration')
	}
}

const vScrollDesc = {
	mounted(el: HTMLElement) {
		const check = () => updateScrollDescMetrics(el)
		nextTick(check)
		setTimeout(check, 100)
		setTimeout(check, 400)
		const ro = new ResizeObserver(check)
		ro.observe(el)
		const inner = el.querySelector('.scrolling-desc-track')
		if (inner) ro.observe(inner)
		const p = inner?.querySelector('p')
		if (p) ro.observe(p)
		;(el as any).__scrollDescRo = ro
		window.addEventListener('resize', check)
		;(el as any).__scrollDescWinResize = check
	},
	updated(el: HTMLElement) {
		const check = () => updateScrollDescMetrics(el)
		nextTick(check)
		setTimeout(check, 100)
	},
	unmounted(el: HTMLElement) {
		const ro = (el as any).__scrollDescRo as ResizeObserver | undefined
		if (ro) ro.disconnect()
		const winCheck = (el as any).__scrollDescWinResize
		if (winCheck) window.removeEventListener('resize', winCheck)
	},
}

// =========================================================
// Settings State & Logic (1:1 with Electron Settings View, Max RAM = 80% of system total)
// =========================================================
const systemTotalRamMb = ref(16384)
const maxAllowedRamMb = computed(() => {
	return Math.max(2048, Math.floor((systemTotalRamMb.value * 0.8) / 512) * 512)
})
const recommendedRamMb = computed(() => {
	const total = systemTotalRamMb.value
	let target = 4096
	if (total <= 6144) target = 2048
	else if (total <= 8192) target = 4096
	else if (total <= 12288) target = 6144
	else if (total <= 20480) target = 8192
	else target = 10240
	return Math.min(target, maxAllowedRamMb.value)
})
const settingsMemoryMb = ref(4096)
const settingsMinMemoryMb = ref(1024)
const settingsJavaAuto = ref(localStorage.getItem('dr3sh.java_auto') !== 'false')
const settingsJavaPath = ref(localStorage.getItem('dr3sh.java_path') || '')
const settingsJvmArgs = ref('')
const settingsWidth = ref(1280)
const settingsHeight = ref(720)
const settingsFullscreen = ref(false)
const settingsCloseOnLaunchMode = ref<'keep' | 'minimize' | 'close'>(
	(localStorage.getItem('dr3sh.close_on_launch') as 'keep' | 'minimize' | 'close') || 'keep',
)
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
		const savedJavaAuto = localStorage.getItem('dr3sh.java_auto')
		const savedJavaPath = (localStorage.getItem('dr3sh.java_path') || '').trim()
		if (savedJavaAuto === 'false' && savedJavaPath) {
			settingsJavaAuto.value = false
			settingsJavaPath.value = savedJavaPath
		} else if (savedJavaAuto === null && matchingInstance.value?.java_path) {
			settingsJavaAuto.value = false
			settingsJavaPath.value = matchingInstance.value.java_path
		} else {
			settingsJavaAuto.value = true
			settingsJavaPath.value = ''
		}
		if (s) {
			rawBackendSettings.value = s
			const savedMax = Number(s.memory?.maximum) || 4096
			const userConfiguredRam = localStorage.getItem('dr3sh.ram_user_configured') === '1'
			if (!userConfiguredRam && savedMax <= 4096 && recommendedRamMb.value !== savedMax) {
				settingsMemoryMb.value = recommendedRamMb.value
				settingsMinMemoryMb.value = Math.min(
					Number(localStorage.getItem('dr3sh.min_ram_mb')) || 1024,
					settingsMemoryMb.value,
				)
				void saveLauncherSettings(false, false)
			} else {
				settingsMemoryMb.value = Math.min(savedMax, maxAllowedRamMb.value)
				settingsMinMemoryMb.value = Math.min(
					Number(localStorage.getItem('dr3sh.min_ram_mb')) || 1024,
					settingsMemoryMb.value,
				)
			}
			settingsJvmArgs.value = Array.isArray(s.extra_launch_args) ? s.extra_launch_args.join(' ') : ''
			settingsWidth.value = s.game_resolution?.[0] || 1280
			settingsHeight.value = s.game_resolution?.[1] || 720
			settingsFullscreen.value = Boolean(s.force_fullscreen)
			if (!localStorage.getItem('dr3sh.close_on_launch') && s.hide_on_process_start) {
				settingsCloseOnLaunchMode.value = 'minimize'
			}
		}
	} catch (e) {
		console.warn('Error loading settings:', e)
	}
}

async function syncJavaOverrideToInstances() {
	const trimmedJavaPath = settingsJavaPath.value.trim()
	const useAuto = settingsJavaAuto.value || !trimmedJavaPath
	const desiredJavaPath = useAuto ? (null as unknown as string) : trimmedJavaPath
	await Promise.all(
		instances.value.map((inst) =>
			editInstance(inst.id, { java_path: desiredJavaPath }).catch(() => {}),
		),
	)
}

async function saveLauncherSettings(notify = false, markUserRam = true) {
	try {
		const clampedRam = Math.min(Number(settingsMemoryMb.value) || 4096, maxAllowedRamMb.value)
		settingsMemoryMb.value = clampedRam
		if (settingsMinMemoryMb.value > clampedRam) {
			settingsMinMemoryMb.value = clampedRam
		}
		if (markUserRam) {
			localStorage.setItem('dr3sh.ram_user_configured', '1')
		}
		const trimmedJavaPath = settingsJavaPath.value.trim()
		const useAutoJava = settingsJavaAuto.value || !trimmedJavaPath
		settingsJavaAuto.value = useAutoJava
		if (useAutoJava) {
			settingsJavaPath.value = ''
		}
		localStorage.setItem('dr3sh.java_auto', String(useAutoJava))
		localStorage.setItem('dr3sh.java_path', useAutoJava ? '' : trimmedJavaPath)
		localStorage.setItem('dr3sh.min_ram_mb', String(settingsMinMemoryMb.value))
		localStorage.setItem('dr3sh.close_on_launch', settingsCloseOnLaunchMode.value)
		localStorage.setItem('dr3sh.default_instances_dir', settingsDefaultInstancesDir.value)

		const base = rawBackendSettings.value || (await getSettings())
		if (base) {
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
				hide_on_process_start: settingsCloseOnLaunchMode.value === 'minimize',
				discord_rpc: false,
			}
			await setSettings(updated)
			rawBackendSettings.value = updated
		}
		await syncJavaOverrideToInstances()
		if (notify) {
			showToast('Ajustes guardados correctamente.')
		}
	} catch (e) {
		console.warn('Error saving settings:', e)
	}
}

function applyRecommendedRam() {
	settingsMemoryMb.value = recommendedRamMb.value
	if (settingsMinMemoryMb.value > settingsMemoryMb.value) {
		settingsMinMemoryMb.value = settingsMemoryMb.value
	}
	localStorage.setItem('dr3sh.ram_user_configured', '1')
	void saveLauncherSettings(true)
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
			await saveLauncherSettings()
			showToast(`Ruta de Java guardada: ${selected}`)
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

const OFFLINE_USERNAME_REGEX = /^[A-Za-z0-9_]{3,16}$/

async function handleOfflineLoginSubmit() {
	const name = offlineUsernameInput.value.trim()
	if (!OFFLINE_USERNAME_REGEX.test(name)) {
		showToast('El nombre offline debe tener entre 3 y 16 caracteres (solo letras, números y _).')
		return
	}
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
		const rawErr = String(e?.message || e || '')
		if (isOffline.value || /network|dns|connect|timeout|fetch|offline/i.test(rawErr)) {
			showToast('No hay conexión a internet para iniciar sesión con Microsoft.')
		} else {
			showToast(`Error de autenticación Microsoft: ${e?.message || e}`)
		}
	} finally {
		busy.value = false
	}
}

// First-launch account onboarding state
const showFirstLaunchAccountModal = ref(false)
const firstLaunchAccountMode = ref<'choose' | 'offline'>('choose')
const firstLaunchOfflineNickname = ref('Player')

async function handleFirstLaunchChooseMicrosoft() {
	showFirstLaunchAccountModal.value = false
	localStorage.setItem('dr3sh.first_launch_done', 'true')
	await handleMicrosoftLogin()
}

function handleFirstLaunchChooseOffline() {
	firstLaunchAccountMode.value = 'offline'
	firstLaunchOfflineNickname.value = 'Player'
}

async function handleFirstLaunchSubmitOffline() {
	const name = (firstLaunchOfflineNickname.value || '').trim() || 'Player'
	if (!OFFLINE_USERNAME_REGEX.test(name)) {
		showToast('El nombre offline debe tener entre 3 y 16 caracteres (solo letras, números y _).')
		return
	}
	showFirstLaunchAccountModal.value = false
	localStorage.setItem('dr3sh.first_launch_done', 'true')
	try {
		busy.value = true
		await loginOfflineAccount(name)
		showToast(`Cuenta sin conexión configurada: ${name}`)
	} catch (e: any) {
		showToast(`Error al configurar cuenta: ${e?.message || e}`)
	} finally {
		busy.value = false
	}
}

function handleFirstLaunchClose() {
	showFirstLaunchAccountModal.value = false
	localStorage.setItem('dr3sh.first_launch_done', 'true')
	if (!activeAccount.value) {
		void loginOfflineAccount('Player').catch(() => {})
	}
}

function handleBeforeUnload() {
	if (testingMode.value && !isDeveloper.value) {
		try {
			const raw = localStorage.getItem('dr3sh.testing_instances')
			const data = raw ? JSON.parse(raw) : { instances: {} }
			data.lastClosedAt = Date.now()
			data.lastHeartbeat = Date.now()
			localStorage.setItem('dr3sh.testing_instances', JSON.stringify(data))
		} catch {}
	}
}

let lastFocusCatalogRefreshAt = 0
let lastAutoCatalogRefreshAt = 0

function handleVisibilityChange() {
	isDocumentHidden.value = typeof document !== 'undefined' ? document.hidden : false
	if (!isDocumentHidden.value && !isDownloading.value) {
		void checkSelectedInstanceHealth()
		void checkRunningState()
		const now = Date.now()
		if (launcherRefreshMode.value === 'automatic' && now - lastFocusCatalogRefreshAt >= 3000) {
			lastFocusCatalogRefreshAt = now
			void fetchVersionsData(true)
		}
	}
}

function handleWindowFocus() {
	isDocumentHidden.value = typeof document !== 'undefined' ? document.hidden : false
	if (!isDownloading.value) {
		void checkSelectedInstanceHealth()
		void checkRunningState()
		const now = Date.now()
		if (launcherRefreshMode.value === 'automatic' && now - lastFocusCatalogRefreshAt >= 3000) {
			lastFocusCatalogRefreshAt = now
			void fetchVersionsData(true)
		}
	}
}

let testingHeartbeatTimer: ReturnType<typeof setInterval> | null = null

onMounted(async () => {
	void fetchVersionsData()
	void loadLauncherSettings()
	window.addEventListener('click', closeInstanceMenu)

	// First-launch account onboarding prompt
	const hasPromptedAccount = localStorage.getItem('dr3sh.first_launch_done')
	if (!hasPromptedAccount) {
		const rawLastUser = localStorage.getItem('dr3sh.last_username')
		if (!rawLastUser && (!activeAccount.value || activeAccount.value.name === 'Player' || activeAccount.value.name === 'fkdr3sh')) {
			showFirstLaunchAccountModal.value = true
		}
	}

	// Gallery auto-advance every 3.8s unless hovered or zoomed (resettable on manual interaction)
	restartGalleryTimer()

	// Poll catalog every 5s during maintenance (so unlock applies fast), or 15s normally
	catalogAutoRefreshTimer = setInterval(() => {
		if (launcherRefreshMode.value !== 'automatic' || isDownloading.value) return
		const now = Date.now()
		const minInterval = maintenanceMode.value ? 5000 : 15000
		if (isDocumentHidden.value && !maintenanceMode.value) return
		if (now - lastAutoCatalogRefreshAt < minInterval) return
		lastAutoCatalogRefreshAt = now
		void fetchVersionsData(maintenanceMode.value)
	}, 5000)

	// Poll running process state every 3s
	runningPollTimer = setInterval(() => {
		void checkRunningState()
	}, 3000)

	// Check and execute testing session cleanup with complete disk and database purge
	if (testingMode.value && !isDeveloper.value) {
		try {
			const raw = localStorage.getItem('dr3sh.testing_instances')
			if (raw) {
				const data = JSON.parse(raw)
				const now = Date.now()
				const hoursLimit = Number(testingHours.value || 1)
				const maxDurationMs = hoursLimit * 60 * 60 * 1000
				const srvMap = (data.instances && typeof data.instances === 'object') ? data.instances : {}
				const serverIds = Object.keys(srvMap)
				let cleanedCount = 0

				for (const srvId of serverIds) {
					const instMeta = srvMap[srvId] || {}
					const installedAt = Number(instMeta.installedAt || data.lastClosedAt || data.lastHeartbeat || 0)
					if (installedAt > 0 && now - installedAt >= maxDurationMs) {
						// Clean up instance both in Theseus database and on disk
						const targetServer = servers.value.find((s) => s.id === srvId)
						const matchingInst = findMatchingInstance(targetServer, instances.value)
						const savedFolder = getInstalledFolder(srvId)

						if (matchingInst?.id) {
							try {
								await remove(matchingInst.id)
							} catch (rmErr) {
								console.warn(`[Testing Mode] Error removing Theseus instance ${matchingInst.id}:`, rmErr)
							}
						}

						try {
							await invoke('dr3sh_cleanup_deleted_instance', {
								serverId: srvId,
								customFolder: savedFolder,
							})
						} catch (cleanErr) {
							console.warn(`[Testing Mode] Error cleaning files for ${srvId}:`, cleanErr)
						}

						removeInstalledVersion(srvId)
						delete srvMap[srvId]
						cleanedCount++
					}
				}

				if (cleanedCount > 0) {
					if (Object.keys(srvMap).length === 0) {
						localStorage.removeItem('dr3sh.testing_instances')
					} else {
						data.instances = srvMap
						localStorage.setItem('dr3sh.testing_instances', JSON.stringify(data))
					}
					showToast(
						`[Modo Testing] La sesión de prueba de ${hoursLimit}h ha finalizado para ${cleanedCount} modpack(s). Se han desinstalado y limpiado completamente del equipo.`,
						6000,
					)
					await instancesQuery.refetch()
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

	window.addEventListener('beforeunload', handleBeforeUnload)
	window.addEventListener('focus', handleWindowFocus)
	window.addEventListener('mouseup', handleZoomMouseUp)
	document.addEventListener('visibilitychange', handleVisibilityChange)
	void checkRunningState()
	void checkSelectedInstanceHealth()
})

onUnmounted(() => {
	window.removeEventListener('click', closeInstanceMenu)
	window.removeEventListener('beforeunload', handleBeforeUnload)
	window.removeEventListener('focus', handleWindowFocus)
	window.removeEventListener('mouseup', handleZoomMouseUp)
	document.removeEventListener('visibilitychange', handleVisibilityChange)
	if (toastTimer) {
		clearTimeout(toastTimer)
		toastTimer = null
	}
	if (galleryAutoTimer) {
		clearInterval(galleryAutoTimer)
		galleryAutoTimer = null
	}
	if (catalogAutoRefreshTimer) {
		clearInterval(catalogAutoRefreshTimer)
		catalogAutoRefreshTimer = null
	}
	if (runningPollTimer) {
		clearInterval(runningPollTimer)
		runningPollTimer = null
	}
	if (testingHeartbeatTimer) {
		clearInterval(testingHeartbeatTimer)
		testingHeartbeatTimer = null
	}
	if (marketHoverTimer) {
		clearInterval(marketHoverTimer)
		marketHoverTimer = null
	}
	stopLogsPolling()
})

const activatingPreloadPromises = new Map<string, Promise<boolean>>()

async function activatePreloadIfNeeded(srv: Dr3shServer, ver: Dr3shVersion): Promise<boolean> {
	const existing = activatingPreloadPromises.get(srv.id)
	if (existing) return existing

	const task = (async () => {
		try {
			await invoke('dr3sh_activate_preload', {
				serverId: srv.id,
				versionRef: {
					id: ver.id,
					version: ver.version,
					download_url: ver.link,
					manifest_url: ver.manifest,
					game_version: ver.minecraft_version || srv.minecraft_version,
					loader: ver.mod_loader || srv.mod_loader,
					loader_version: ver.mod_loader_version || srv.mod_loader_version,
					sha256: ver.sha256,
					size_mb: ver.size_mb,
					server_name: srv.title,
					icon_url: srv.logo_square || srv.logo || srv.image || null,
					patch: ver.patch || null,
				},
				expectedReleaseDate: srv.release_date || null,
				bypassGate: Boolean(isDeveloper.value),
			})
			addOwned(srv.id)
			await refreshPreloadStatus(srv.id)
			await instancesQuery.refetch()
			await checkSelectedInstanceHealth()
			return true
		} catch (e: any) {
			console.warn('Auto-activate preload failed:', e)
			return false
		} finally {
			activatingPreloadPromises.delete(srv.id)
		}
	})()

	activatingPreloadPromises.set(srv.id, task)
	return task
}

watch(
	[
		() => selectedServer.value?.id,
		isCountdownActive,
		() => preloadStatus.value.preloaded,
		isInstalled,
	],
	() => {
		const srv = selectedServer.value
		const ver = selectedVersion.value
		if (!srv || !ver) return
		if (!preloadStatus.value.preloaded || isInstalled.value) return
		if (isCountdownActive.value && !isDeveloper.value) return
		if (isDownloading.value || busy.value) return
		void activatePreloadIfNeeded(srv, ver)
	},
	{ immediate: true },
)

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

	if (buttonAction.value === 'disabled') {
		showToast(
			selectedServer.value?.disabled_message ||
				'Este modpack no se encuentra disponible actualmente.',
			4500,
		)
		return
	}

	if (buttonAction.value === 'countdown') {
		showToast(`Este modpack no está disponible hasta el inicio del server: ${countdownLabel.value}`, 4500)
		return
	}

	if (buttonAction.value === 'preloaded') {
		showToast('Modpack precargado y protegido. Se desbloqueará al iniciar el server.', 4500)
		return
	}

	if (buttonAction.value === 'preload' && selectedServer.value && selectedVersion.value) {
		try {
			busy.value = true
			const srv = selectedServer.value
			const ver = selectedVersion.value
			showToast(`Precargando ${srv.title} v${ver.version} en modo protegido...`)
			await downloadAndInstallModpack({
				server: srv,
				version: ver,
				bypassGate: true,
				onSuccess: async (done) => {
					await invoke('dr3sh_lock_preload', {
						serverId: srv.id,
						instanceId: done.instance_id,
						instanceDir: done.instance_path,
						version: done.version || ver.version || '1.0.0',
						releaseDate: srv.release_date || null,
					})
					await refreshPreloadStatus(srv.id)
					await instancesQuery.refetch()
					showToast(`¡${srv.title} precargado y protegido! Se activará en 1 segundo al estrenarse.`, 5500)
				},
				onError: (err) => {
					openErrorDiagnosticsModal(err)
				},
			})
		} catch (e: any) {
			openErrorDiagnosticsModal(e)
		} finally {
			busy.value = false
		}
		return
	}

	if (
		(buttonAction.value === 'activate' ||
			(buttonAction.value === 'play' && preloadStatus.value.preloaded && !isInstalled.value)) &&
		selectedServer.value &&
		selectedVersion.value
	) {
		try {
			busy.value = true
			const srv = selectedServer.value
			const ver = selectedVersion.value
			const ok = await activatePreloadIfNeeded(srv, ver)
			if (!ok) {
				showToast(`No se pudo desbloquear la precarga de ${srv.title}. Inténtalo de nuevo.`, 4500)
				return
			}
			if (buttonAction.value === 'activate') {
				showToast(`¡${srv.title} activado al instante! Ya puedes jugar.`, 4500)
				return
			}
		} catch (e: any) {
			openErrorDiagnosticsModal(e)
			return
		} finally {
			busy.value = false
		}
	}

	if (buttonAction.value === 'repair') {
		await repairInstance()
		return
	}

	if (buttonAction.value === 'play' && matchingInstance.value) {
		try {
			busy.value = true
			notice.value = null
			await checkSelectedInstanceHealth()
			if (selectedInstanceHealth.value.needs_repair) {
				showToast(
					selectedInstanceHealth.value.reason ||
						'Faltan archivos vitales de la instancia. Repárala antes de jugar.',
					5000,
				)
				return
			}
			if (!activeAccount.value) {
				await refreshAccount()
			}
			if (selectedServer.value) {
				await invoke('dr3sh_prepare_launch', {
					serverId: selectedServer.value.id,
					isOffline: !activeAccount.value?.isMicrosoft,
					offlineUsername: activeAccount.value?.name || 'Player',
					bypassGate: Boolean(isDeveloper.value),
				})
				const savedFolder = getInstalledFolder(selectedServer.value.id)
				if (savedFolder) {
					const instDir = await get_full_path(matchingInstance.value.id).catch(() => null)
					if (instDir && instDir.toLowerCase() !== savedFolder.toLowerCase()) {
						await invoke('dr3sh_link_existing_instance', {
							instanceId: matchingInstance.value.id,
							instanceDir: instDir,
							existingFolder: savedFolder,
						}).catch(() => {})
					}
				}
			}
			const trimmedJava = settingsJavaPath.value.trim()
			const desiredJavaPath =
				!settingsJavaAuto.value && trimmedJava
					? trimmedJava
					: (null as unknown as string)
			await editInstance(matchingInstance.value.id, {
				java_path: desiredJavaPath,
			}).catch(() => {})
			lastLaunchStartedAtSec.value[matchingInstance.value.id] = Math.floor(Date.now() / 1000)
			await run(matchingInstance.value.id)
			isSelectedRunning.value = true
			showToast(`Iniciando ${matchingInstance.value.name}...`)
			if (settingsCloseOnLaunchMode.value === 'minimize') {
				await getCurrentWindow().minimize().catch(() => {})
			} else if (settingsCloseOnLaunchMode.value === 'close') {
				await getCurrentWindow().close().catch(() => {})
			}
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
			const prevCustomFolder = getInstalledFolder(srv.id)
			showToast(`Actualizando ${srv.title} a la versión ${ver.version}...`)
			await downloadAndInstallModpack({
				server: srv,
				version: ver,
				matchingInstanceId: matchingInstance.value?.id,
				customFolder: prevCustomFolder,
				bypassGate: Boolean(isDeveloper.value),
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
					openErrorDiagnosticsModal(err)
				},
			})
		} catch (e: any) {
			openErrorDiagnosticsModal(e)
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
	if (!matchingInstance.value || killingInstance.value) return
	try {
		killingInstance.value = true
		await killInstance(matchingInstance.value.id)
		await checkRunningState()
		showToast(`Se ha forzado el cierre de ${selectedServer.value?.title || 'Minecraft'}.`)
	} catch (e: any) {
		showToast(`Error al cerrar instancia: ${e?.message || e}`)
	} finally {
		killingInstance.value = false
	}
}

async function handleCancelDownload() {
	if (isCancelling.value) return
	showToast('Cancelando operación...')
	await cancelActiveDownload()
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

async function handleCreateDesktopShortcut() {
	if (!selectedServer.value || !matchingInstance.value) return
	try {
		const srv = selectedServer.value
		const inst = matchingInstance.value
		const rawIcon = srv.logo_square || srv.logo || srv.image
		const iconUrl = rawIcon ? resolveImageUrl(rawIcon) : null
		const createdPath = await invoke<string>('dr3sh_create_desktop_shortcut', {
			instanceName: srv.title,
			instanceId: inst.id,
			serverId: srv.id,
			iconUrl,
		})
		showToast(`Acceso directo creado en el escritorio (${createdPath})`)
	} catch (err: any) {
		console.error('Error creando acceso directo:', err)
		showToast(`Error creando acceso directo: ${err?.message || err}`)
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
			bypassGate: Boolean(isDeveloper.value),
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
				openErrorDiagnosticsModal(err)
			},
		})
	} catch (err: any) {
		console.error('Failed to install modpack:', err)
		openErrorDiagnosticsModal(err)
	} finally {
		busy.value = false
	}
}

async function openInstanceFolder() {
	const customPath = selectedServer.value
		? getInstalledFolder(selectedServer.value.id)
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
			await repairModpack({
				server: selectedServer.value,
				version: selectedVersion.value,
				onSuccess: async () => {
					await instancesQuery.refetch()
					showToast('Verificación y reparación completada.')
				},
				onError: (err) => {
					openErrorDiagnosticsModal(err)
				},
			})
		} catch (e: any) {
			console.error('Repair failed:', e)
			openErrorDiagnosticsModal(e)
		} finally {
			busy.value = false
		}
	}
}

async function handleExecuteDeleteConfirm() {
	if (!deleteConfirmModal.value) return
	const { server, mode } = deleteConfirmModal.value
	const inst = findInstanceForServer(server)

	if (inst?.id) {
		try {
			const procs = await get_by_instance_id(inst.id)
			if (Array.isArray(procs) && procs.length > 0) {
				closeDeleteConfirm()
				showToast('No puedes eliminar una instancia mientras está en ejecución. Ciérrala primero.', 5000)
				return
			}
		} catch {}
	}

	closeDeleteConfirm()

	try {
		busy.value = true
		const savedCustomFolder = getInstalledFolder(server.id)
		const inst = findInstanceForServer(server)
		if (inst) {
			await remove(inst.id)
		}
		await invoke('dr3sh_cleanup_deleted_instance', {
			serverId: server.id,
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

	<Transition :name="isPowerSaving ? '' : 'dr3sh-server-fade'" mode="out-in">
	<!-- =========================================================
	     VIEW 1: HOME (BIBLIOTECA DE INSTANCIAS)
	     ========================================================= -->
	<div
		v-if="currentView === 'home' && selectedServer"
		key="home"
		class="home-layout obsidian-home"
		:class="{ 'is-power-saving': isPowerSaving }"
	>
		<!-- 1. CAPTURA PANORÁMICA DE FONDO CON FADE PURO (ESTÁTICA, SIN REBOTE) -->
		<div
			class="hero-slideshow-stack"
			@mouseenter="isHoveringGallery = true"
			@mouseleave="isHoveringGallery = false"
		>
			<Transition :name="isPowerSaving ? '' : 'dr3sh-bg-fade'">
				<div :key="`bg-server-${selectedServer.id}`" class="hero-bg-layer">
					<div
						v-for="(s, idx) in previewImages"
						:key="`bg-${selectedServer.id}-${idx}`"
						class="slide-fade-image"
						:class="{ 'is-active': idx === activeScreenshotIndex }"
						:style="{ backgroundImage: `url(&quot;${getStageImageUrl(s)}&quot;)` }"
					/>
				</div>
			</Transition>

			<!-- Viñeta oscura cinematográfica y resplandor ambiental -->
			<div class="slideshow-vignette-top" />
			<div class="slideshow-vignette-bottom" />
			<div class="slideshow-ambient-glow" />
		</div>

		<div :key="`fg-${selectedServer.id}`" class="hero-foreground-layer">
				<!-- 2. HEADER DEL MODPACK (SIN PÍLDORA 'AL DÍA', SIN VERSIÓN DUPLICADA ARRIBA) -->
		<div class="hero-top-row">
			<div class="hero-meta-col">
				<div class="hero-logo-slot">
					<img
						v-if="selectedServer.title_logo && !titleLogoFailed"
						ref="titleLogoImgRef"
						v-show="titleLogoLoaded"
						:key="`${selectedServer.id}:${selectedServer.title_logo}`"
						:src="resolveImageUrl(selectedServer.title_logo)"
						:alt="selectedServer.title"
						class="hero-modpack-title-logo"
						@load="titleLogoLoaded = true; titleLogoFailed = false"
						@error="titleLogoFailed = true"
					/>
					<h1 v-if="!selectedServer.title_logo || titleLogoFailed || !titleLogoLoaded" class="hero-modpack-title">
						{{ selectedServer.title }}
					</h1>
				</div>
				<div
					v-if="selectedServer.description"
					v-scroll-desc="selectedServer.description"
					class="hero-desc-scroll-box"
				>
					<div class="scrolling-desc-track">
						<p>{{ selectedServer.description }}</p>
					</div>
				</div>
			</div>

			<!-- MENÚ DE 3 PUNTOS (CONTIENE ACCESO DIRECTO SI ESTÁ DESCARGADO) -->
			<div class="instance-menu-anchor" @click.stop>
				<button
					type="button"
					class="hero-menu-trigger"
					:class="{ active: instanceMenuOpen }"
					title="Opciones de la instancia"
					@click="toggleInstanceMenu"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true" class="w-4 h-4 fill-current">
						<circle cx="10" cy="4.5" r="1.6" />
						<circle cx="10" cy="10" r="1.6" />
						<circle cx="10" cy="15.5" r="1.6" />
					</svg>
				</button>

				<!-- Dropdown Flotante -->
				<div v-if="instanceMenuOpen" class="instance-dropdown-menu" role="menu">
					<div class="instance-menu-header">
						<span class="instance-menu-header-icon">
							<img
								:src="resolveImageUrl(selectedServer.logo_square || selectedServer.logo || selectedServer.image)"
								:alt="selectedServer.title"
								@error="handleImageError($event)"
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
						v-if="isSelectedRunning && matchingInstance"
						type="button"
						class="instance-menu-item danger"
						:disabled="killingInstance"
						@click="handleKillRunningInstance(); closeInstanceMenu()"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true">
							<rect x="4.5" y="4.5" width="11" height="11" rx="2" fill="currentColor" />
						</svg>
						<span>{{ killingInstance ? 'Cerrando Minecraft...' : 'Matar proceso de Minecraft' }}</span>
					</button>

					<button
						v-if="matchingInstance"
						type="button"
						class="instance-menu-item"
						@click="handleCreateDesktopShortcut(); closeInstanceMenu()"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true">
							<path d="M3 4.5A1.5 1.5 0 0 1 4.5 3h11A1.5 1.5 0 0 1 17 4.5v8a1.5 1.5 0 0 1-1.5 1.5H4.5A1.5 1.5 0 0 1 3 12.5v-8ZM7 17h6M10 14v3" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
						</svg>
						<span>Crear acceso directo en el escritorio</span>
					</button>

					<button
						v-if="matchingInstance"
						type="button"
						class="instance-menu-item"
						@click="openInstanceLogsModal(); closeInstanceMenu()"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true">
							<path d="M3.5 4.5A1.5 1.5 0 0 1 5 3h10a1.5 1.5 0 0 1 1.5 1.5v11A1.5 1.5 0 0 1 15 17H5a1.5 1.5 0 0 1-1.5-1.5v-11ZM6.5 7.5l2.5 2-2.5 2m4 0H14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
						</svg>
						<span>Ver consola y logs</span>
					</button>

					<button
						type="button"
						class="instance-menu-item"
						:disabled="busy || isDownloading"
						@click="repairInstance(); closeInstanceMenu()"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true">
							<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l2-2a5.5 5.5 0 0 1-7.4 7.4l-6.2 6.2a1.8 1.8 0 0 1-2.6-2.6l6.2-6.2a5.5 5.5 0 0 1 7.4-7.4l-2.4 1.6Z" />
						</svg>
						<span>Verificar y reparar instalación</span>
					</button>

					<button
						type="button"
						class="instance-menu-item"
						@click="openInstanceFolder(); closeInstanceMenu()"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true">
							<path d="M2.5 5.5A1.5 1.5 0 0 1 4 4h4.2l1.8 2H16a1.5 1.5 0 0 1 1.5 1.5v7A1.5 1.5 0 0 1 16 16H4a1.5 1.5 0 0 1-1.5-1.5v-9Z" />
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
							<path d="M4 6h12M7.5 6V4.5A1.5 1.5 0 0 1 9 3h2a1.5 1.5 0 0 1 1.5 1.5V6m2 0v9.5a1.5 1.5 0 0 1-1.5 1.5H7a1.5 1.5 0 0 1-1.5-1.5V6h9Z" />
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

		<!-- 3. SLIDESHOW DE MINIATURAS: PEGADO ABAJO JUSTO SOBRE EL FOOTER (HASTA 6 MINIATURAS POR TANDA) -->
		<div
			v-if="previewImages.length > 0"
			class="hero-thumbnails-bar"
		>
			<div class="hero-thumbnails-capsule">
				<button
					v-if="previewImages.length > 1"
					type="button"
					class="batch-nav-btn prev"
					title="Captura anterior"
					@click="prevSlide"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true" class="w-4 h-4 fill-none stroke-current" stroke-width="2">
						<path d="M12.5 15 7.5 10l5-5" stroke-linecap="round" stroke-linejoin="round" />
					</svg>
				</button>

				<div class="thumbnails-track">
					<button
						v-for="(shot, idxInBatch) in previewImages.slice(slideBatchStart, slideBatchStart + 6)"
						:key="`thumb-${slideBatchStart + idxInBatch}`"
						type="button"
						class="batch-thumb-item"
						:class="{ active: (slideBatchStart + idxInBatch) === activeScreenshotIndex }"
						:title="(slideBatchStart + idxInBatch) === activeScreenshotIndex ? 'Clic para ampliar captura' : `Ver captura ${slideBatchStart + idxInBatch + 1}`"
						@click="selectScreenshot(slideBatchStart + idxInBatch)"
					>
						<img
							:src="getThumbnailUrl(shot)"
							:alt="`Captura ${slideBatchStart + idxInBatch + 1}`"
							loading="lazy"
							@error="handleImageError($event, shot)"
						/>
					</button>
				</div>

				<button
					v-if="previewImages.length > 1"
					type="button"
					class="batch-nav-btn next"
					title="Captura siguiente"
					@click="nextSlide"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true" class="w-4 h-4 fill-none stroke-current" stroke-width="2">
						<path d="M7.5 5l5 5-5 5" stroke-linecap="round" stroke-linejoin="round" />
					</svg>
				</button>
			</div>
		</div>

		<!-- 3.5. PANEL DE DESCARGA E INSTALACIÓN (OBSIDIAN GLASS) -->
		<div
			v-if="isDownloading && currentServerId === selectedServer.id"
			class="hero-download-card"
			:class="{ 'is-reconnecting': (downloadProgress.attempt ?? 0) > 0 }"
			aria-live="polite"
		>
			<div class="hero-dl-header">
				<div class="hero-dl-status">
					<span class="hero-dl-icon-wrap" aria-hidden="true">
						<svg
							v-if="
								downloadProgress.phase === 'downloading' ||
								downloadProgress.phase === 'preparing'
							"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
							stroke-linejoin="round"
							class="hero-dl-svg hero-dl-svg-pulse"
						>
							<path d="M12 3v12" />
							<path d="m7 10 5 5 5-5" />
							<path d="M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" />
						</svg>
						<svg
							v-else-if="downloadProgress.phase === 'verifying'"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
							stroke-linejoin="round"
							class="hero-dl-svg hero-dl-svg-pulse"
						>
							<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10Z" />
							<path d="m9 12 2 2 4-4" />
						</svg>
						<svg
							v-else-if="downloadProgress.phase === 'extracting'"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
							stroke-linejoin="round"
							class="hero-dl-svg hero-dl-svg-pulse"
						>
							<path
								d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z"
							/>
							<path d="m3.3 7 8.7 5 8.7-5" />
							<path d="M12 22V12" />
						</svg>
						<svg
							v-else
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
							stroke-linejoin="round"
							class="hero-dl-svg hero-dl-svg-pulse"
						>
							<path d="M20 6 9 17l-5-5" />
						</svg>
					</span>
					<span v-if="downloadProgress.stageLabel" class="hero-dl-stage-badge">
						{{ downloadProgress.stageLabel }}
					</span>
					<span class="hero-dl-phase-text">{{ downloadProgress.phaseText }}</span>
				</div>

				<div class="hero-dl-actions">
					<span class="hero-dl-percent">{{ downloadProgress.percent }}%</span>
					<button
						v-if="
							downloadProgress.phase !== 'applying' &&
							downloadProgress.phase !== 'finishing'
						"
						type="button"
						class="hero-dl-cancel-btn"
						:disabled="isCancelling"
						@click="handleCancelDownload"
					>
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
							stroke-linejoin="round"
							width="13"
							height="13"
						>
							<path d="M18 6 6 18" />
							<path d="m6 6 12 12" />
						</svg>
						<span>{{ isCancelling ? 'Cancelando...' : 'Cancelar' }}</span>
					</button>
				</div>
			</div>

			<div class="hero-dl-track">
				<span
					class="hero-dl-fill"
					:style="{ width: `${Math.max(3, downloadProgress.percent)}%` }"
				/>
			</div>

			<div class="hero-dl-footer">
				<div class="hero-dl-metrics">
					<span
						v-if="
							(downloadProgress.phase === 'preparing' ||
								downloadProgress.phase === 'downloading') &&
							downloadProgress.totalBytes >= 1024 &&
							downloadProgress.downloadedFormatted &&
							downloadProgress.totalFormatted
						"
						class="hero-dl-metric"
					>
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							width="12"
							height="12"
						>
							<ellipse cx="12" cy="5" rx="9" ry="3" />
							<path d="M3 5v14a9 3 0 0 0 18 0V5" />
							<path d="M3 12a9 3 0 0 0 18 0" />
						</svg>
						<span>
							{{ downloadProgress.downloadedFormatted }} /
							{{ downloadProgress.totalFormatted }}
						</span>
					</span>
					<span
						v-if="
							(downloadProgress.phase === 'verifying' ||
								downloadProgress.phase === 'extracting' ||
								downloadProgress.phase === 'applying') &&
							downloadProgress.filesTotal
						"
						class="hero-dl-metric"
					>
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							width="12"
							height="12"
						>
							<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
							<path d="M14 2v4a2 2 0 0 0 2 2h4" />
						</svg>
						<span>
							{{ downloadProgress.filesDone ?? 0 }} /
							{{ downloadProgress.filesTotal }} archivos
						</span>
					</span>
					<span
						v-if="
							downloadProgress.phase === 'downloading' &&
							downloadProgress.speedFormatted
						"
						class="hero-dl-metric hero-dl-metric-accent"
					>
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							width="12"
							height="12"
						>
							<path d="m12 14 4-4" />
							<path d="M3.34 19a10 10 0 1 1 17.32 0" />
						</svg>
						<span>{{ downloadProgress.speedFormatted }}</span>
					</span>
					<span
						v-if="
							(downloadProgress.phase === 'downloading' ||
								downloadProgress.phase === 'extracting') &&
							downloadProgress.etaFormatted
						"
						class="hero-dl-metric"
					>
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							width="12"
							height="12"
						>
							<circle cx="12" cy="12" r="10" />
							<polyline points="12 6 12 12 16 14" />
						</svg>
						<span>{{ downloadProgress.etaFormatted }} restantes</span>
					</span>
				</div>

				<div class="hero-dl-stepper" aria-label="Etapas de instalación">
					<span
						v-for="(stepName, idx) in ['Descargar', 'Verificar', 'Extraer', 'Instalar']"
						:key="stepName"
						class="hero-dl-step"
						:class="{
							'is-done': downloadProgress.stepIndex > idx + 1,
							'is-active': downloadProgress.stepIndex === idx + 1,
						}"
					>
						<span class="hero-dl-step-dot" aria-hidden="true" />
						<span>{{ stepName }}</span>
					</span>
				</div>
			</div>
		</div>

		<!-- 4. ACTION FOOTER DOCK (VERSIÓN + MODLOADER + BOTÓN GIGANTE JUGAR) -->
		<div class="hero-action-dock">
			<div class="dock-tech-info">
				<!-- Versión instalada -->
				<div class="dock-meta-item">
					<span class="dock-meta-label">VERSIÓN</span>
					<div class="dock-meta-value">
						<span
							class="dock-status-dot"
							:class="{
								'is-installed': isInstalled && !selectedInstanceHealth.needs_repair,
								'needs-repair': selectedInstanceHealth.needs_repair,
							}"
						/>
						<span>
							{{
								selectedInstanceHealth.needs_repair
									? 'Requiere reparación'
									: isInstalled
										? selectedVersion?.version
											? `v${selectedVersion.version.replace(/^v/i, '')}`
											: 'Instalado'
										: 'Sin instalar'
							}}
						</span>
					</div>
				</div>

				<div class="dock-separator" />

				<!-- Modloader oficial (sin versión) -->
				<div class="dock-meta-item">
					<span class="dock-meta-label">MODLOADER</span>
					<div class="dock-meta-value">
						<img :src="loaderIconUrl(selectedServer.mod_loader)" alt="" class="dock-loader-icon" />
						<span>{{ formatLoaderName(selectedServer.mod_loader) }}</span>
					</div>
				</div>
			</div>

			<!-- Botón Gigante Jugar Ahora / Matar Proceso -->
			<div class="hero-action-buttons">
				<button
					type="button"
					class="hero-play-button"
					:class="[`action-${buttonAction}`, { 'is-running': buttonAction === 'running', 'is-disabled-server': buttonAction === 'disabled', 'is-countdown': buttonAction === 'countdown' || buttonAction === 'preloaded', 'is-preload': buttonAction === 'preload' }]"
					:disabled="busy || isDownloading || buttonAction === 'running' || buttonAction === 'disabled' || buttonAction === 'countdown' || buttonAction === 'preloaded'"
					:title="buttonAction === 'repair' && selectedInstanceHealth.reason ? selectedInstanceHealth.reason : undefined"
					@click="handlePlayOrInstall"
				>
					<template v-if="buttonAction === 'running'">
						<span class="running-dot-indicator" aria-hidden="true" />
					</template>
					<svg
						v-else-if="buttonAction === 'repair'"
						viewBox="0 0 20 20"
						aria-hidden="true"
						class="play-btn-icon"
					>
						<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l2-2a5.5 5.5 0 0 1-7.4 7.4l-6.2 6.2a1.8 1.8 0 0 1-2.6-2.6l6.2-6.2a5.5 5.5 0 0 1 7.4-7.4l-2.4 1.6Z" fill="currentColor" stroke="none" />
					</svg>
					<svg
						v-else-if="buttonAction === 'update'"
						viewBox="0 0 20 20"
						aria-hidden="true"
						class="play-btn-icon"
					>
						<path d="M10 15V5m0 0-4 4m4-4 4 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
					</svg>
					<svg
						v-else-if="buttonAction === 'download'"
						viewBox="0 0 20 20"
						aria-hidden="true"
						class="play-btn-icon"
					>
						<path d="M10 3.5v9m0 0-3.5-3.5M10 12.5 13.5 9M4 15.5h12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
					</svg>
					<svg
						v-else-if="buttonAction === 'countdown'"
						viewBox="0 0 20 20"
						aria-hidden="true"
						class="play-btn-icon"
					>
						<circle cx="10" cy="10" r="7" fill="none" stroke="currentColor" stroke-width="1.8" />
						<polyline points="10 6 10 10 13 12" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
					</svg>
					<svg
						v-else-if="buttonAction === 'disabled'"
						viewBox="0 0 20 20"
						aria-hidden="true"
						class="play-btn-icon"
					>
						<circle cx="10" cy="10" r="7" fill="none" stroke="currentColor" stroke-width="1.8" />
						<line x1="5" y1="5" x2="15" y2="15" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
					</svg>
					<svg v-else viewBox="0 0 20 20" aria-hidden="true" class="play-btn-icon">
						<path d="M6 4.5 15.5 10 6 15.5V4.5Z" fill="currentColor" stroke="none" />
					</svg>
					<span>{{ buttonLabel }}</span>
				</button>

				<button
					v-if="buttonAction === 'running'"
					type="button"
					class="hero-kill-button"
					:disabled="killingInstance"
					title="Forzar cierre del proceso de Minecraft"
					@click="handleKillRunningInstance"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true" class="play-btn-icon">
						<rect x="4.5" y="4.5" width="11" height="11" rx="2" fill="currentColor" />
					</svg>
					<span>{{ killingInstance ? 'CERRANDO...' : 'MATAR PROCESO' }}</span>
				</button>
			</div>
		</div>
		</div>
	</div>

	<!-- Welcome / Empty Library State (Exact from Electron) -->
	<div v-else-if="currentView === 'home' && !selectedServer" key="home-welcome" class="welcome-layout">
		<div v-if="servers.length === 0 && (isOffline || catalogError)" class="welcome-hero">
			<div class="offline-empty-icon" aria-hidden="true">
				<svg
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
					width="26"
					height="26"
				>
					<path d="M12 20h.01" />
					<path d="M8.5 16.429a5 5 0 0 1 7 0" />
					<path d="M5 12.859a10 10 0 0 1 5.17-2.69" />
					<path d="M19 12.859a10 10 0 0 0-2.007-1.523" />
					<path d="M2 8.82a15 15 0 0 1 4.177-2.643" />
					<path d="M22 8.82a15 15 0 0 0-11.288-3.764" />
					<path d="m2 2 20 20" />
				</svg>
			</div>
			<h1 class="welcome-title">Sin conexión a internet</h1>
			<p class="welcome-subtitle">
				No se pudo cargar el catálogo de modpacks. Comprueba tu conexión e inténtalo de nuevo.
			</p>
			<div class="welcome-actions">
				<button
					type="button"
					class="welcome-cta-button"
					:disabled="retryingCatalog"
					@click="handleRetryCatalog"
				>
					<svg
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						class="welcome-btn-icon"
					>
						<path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
						<path d="M3 3v5h5" />
						<path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16" />
						<path d="M16 16h5v5" />
					</svg>
					<span>{{ retryingCatalog ? 'REINTENTANDO...' : 'REINTENTAR CONEXIÓN' }}</span>
				</button>
			</div>
		</div>
		<div v-else class="welcome-hero">
			<div class="welcome-logo-badge">
				<img src="/logo.png" alt="Dr3sh App" class="welcome-logo" />
			</div>
			<h1 class="welcome-title">Dr3sh App</h1>
			<p class="welcome-subtitle">
				Selecciona un modpack de tu biblioteca para ver sus detalles o explora el catálogo para añadir nuevos servidores.
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
	</div>

	<!-- =========================================================
	     VIEW 2: MARKET (EXPLORAR MODPACKS)
	     ========================================================= -->
	<div v-else-if="currentView === 'market'" key="market" class="market-layout obsidian-market">
		<div class="market-header-row">
			<div class="market-heading-text">
				<h1>Explorar modpacks</h1>
				<p>Añade nuevos modpacks a tu biblioteca con un solo clic</p>
			</div>

			<!-- LUPA DESPLEGABLE CON ANIMACIÓN SUAVE HACIA LA IZQUIERDA -->
			<div class="market-header-actions">
				<div class="expandable-search-wrapper" :class="{ 'is-expanded': isSearchExpanded }">
					<input
						ref="marketSearchInput"
						v-model="searchQuery"
						type="search"
						placeholder="Buscar modpack"
						class="search-input-field"
					/>
					<button
						type="button"
						class="search-toggle-btn"
						title="Buscar modpack"
						@click="toggleSearchExpand"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true" class="w-4 h-4 fill-none stroke-current" stroke-width="2">
							<circle cx="9" cy="9" r="6" stroke-linecap="round" stroke-linejoin="round" />
							<path d="m13.5 13.5 4 4" stroke-linecap="round" stroke-linejoin="round" />
						</svg>
					</button>
				</div>

				<button
					type="button"
					class="market-back-btn"
					@click="currentView = 'home'"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true" class="w-4 h-4 fill-none stroke-current" stroke-width="2">
						<path d="M12.5 15 7.5 10l5-5" stroke-linecap="round" stroke-linejoin="round" />
					</svg>
					<span>Volver</span>
				</button>
			</div>
		</div>

		<div class="market-grid">
			<article
				v-for="server in filteredMarketServers"
				:key="server.id"
				class="market-card-item"
				:style="{
					'--card-accent': getAccentColor(server),
					'--card-on-accent': getContrastTextColor(getAccentColor(server)),
					'--card-glow': `color-mix(in srgb, ${getAccentColor(server)} 42%, transparent)`,
				}"
				@mouseenter="hoveredMarketId = server.id"
				@mouseleave="hoveredMarketId = null"
			>
				<div class="market-card-banner">
					<img
						:src="
							server.explore_image
								? resolveImageUrl(server.explore_image)
								: getThumbnailUrl(getServerPreviewImages(server)[0] || resolveImageUrl(server.image || server.logo || server.logo_square))
						"
						:alt="server.title"
						class="market-card-img"
						@error="
							handleImageError(
								$event,
								server.explore_image
									? resolveImageUrl(server.explore_image)
									: getServerPreviewImages(server)[0] || resolveImageUrl(server.image || server.logo || server.logo_square),
							)
						"
					/>
					<span class="market-loader-badge">
						<img :src="loaderIconUrl(server.mod_loader)" alt="" class="market-loader-icon" />
						<span>{{ formatLoaderName(server.mod_loader) }} • {{ server.minecraft_version || '1.21.1' }}</span>
					</span>
				</div>

				<div class="market-card-body">
					<div class="market-card-head">
						<span class="market-card-icon">
							<img
								v-if="server.logo_square || server.logo || server.image"
								:src="resolveImageUrl(server.logo_square || server.logo || server.image)"
								:alt="server.title"
								@error="handleImageError($event)"
							/>
							<span v-else class="market-card-icon-fallback">
								{{ server.title.slice(0, 2) }}
							</span>
						</span>
						<div class="market-card-title-wrap">
							<h2 class="market-card-title">{{ server.title }}</h2>
							<span
								class="market-status-tag"
								:class="{ 'in-library': owned.has(server.id) }"
							>
								{{ owned.has(server.id) ? '● En biblioteca' : '● Disponible' }}
							</span>
						</div>
					</div>

					<p class="market-card-desc">
						{{ server.description || 'Disponible para instalar en tu biblioteca.' }}
					</p>

					<div class="market-card-footer">
						<button
							v-if="owned.has(server.id)"
							type="button"
							class="card-remove-btn card-full-btn"
							@click="requestDeleteConfirm(server, 'pack-remove')"
						>
							Eliminar
						</button>
						<button
							v-else
							type="button"
							class="card-add-btn card-full-btn"
							@click="addOwned(server.id); showToast(`Modpack &quot;${server.title}&quot; añadido a tu biblioteca.`)"
						>
							Obtener
						</button>
					</div>
				</div>
			</article>
		</div>

		<div v-if="loading && servers.length === 0" class="no-results">
			Cargando catálogo de modpacks...
		</div>
		<div v-else-if="servers.length === 0 && (isOffline || catalogError)" class="offline-empty-card">
			<div class="offline-empty-icon" aria-hidden="true">
				<svg
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
					width="24"
					height="24"
				>
					<path d="M12 20h.01" />
					<path d="M8.5 16.429a5 5 0 0 1 7 0" />
					<path d="M5 12.859a10 10 0 0 1 5.17-2.69" />
					<path d="M19 12.859a10 10 0 0 0-2.007-1.523" />
					<path d="M2 8.82a15 15 0 0 1 4.177-2.643" />
					<path d="M22 8.82a15 15 0 0 0-11.288-3.764" />
					<path d="m2 2 20 20" />
				</svg>
			</div>
			<h3>Sin conexión a internet</h3>
			<p>No se pudo descargar la lista de modpacks. Revisa tu conexión e inténtalo de nuevo.</p>
			<button
				type="button"
				class="offline-retry-btn"
				:disabled="retryingCatalog"
				@click="handleRetryCatalog"
			>
				{{ retryingCatalog ? 'Reintentando...' : 'Reintentar conexión' }}
			</button>
		</div>
		<div v-else-if="filteredMarketServers.length === 0" class="no-results">
			No se encontraron modpacks con ese nombre.
		</div>
	</div>

	<!-- =========================================================
	     VIEW 3: SETTINGS (AJUSTES DEL LAUNCHER - 5 TABS)
	     ========================================================= -->
	<div v-else-if="currentView === 'settings'" key="settings" class="settings-layout">
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
				</div>
			</div>

			<section class="settings-section">
				<div class="settings-title">
					<div>
						<b>Memoria RAM de Minecraft</b>
						<small>Selecciona cuánta memoria RAM asignar a tus instancias</small>
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
							type="button"
							class="settings-chip-btn"
							:class="{ active: settingsMemoryMb === recommendedRamMb }"
							@click="applyRecommendedRam"
						>
							Recomendado ({{ (recommendedRamMb / 1024).toFixed(recommendedRamMb % 1024 === 0 ? 0 : 1) }} GB)
						</button>
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
											@error="handleAvatarError"
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
									placeholder="Tu nombre en Minecraft (ej. Player)"
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
											@error="handleAvatarError"
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
							<b>Carpeta de datos de Dr3sh App</b>
							<small>Configuración general, cachés y respaldos del launcher</small>
						</div>
						<span>Abrir carpeta</span>
					</button>
				</div>
			</section>
		</div>
	</div>
	</Transition>

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
								spellcheck="false"
								class="install-folder-input"
								placeholder="Ej: D:\Juegos\Minecraft o usa Examinar..."
								@focus="installLocationModal.mode = 'custom'"
								@input="installLocationModal.mode = 'custom'"
							/>
							<button
								type="button"
								class="install-browse-btn"
								@click="browseCustomFolder"
							>
								<svg viewBox="0 0 20 20" aria-hidden="true" class="w-4 h-4 fill-none stroke-current" stroke-width="1.8">
									<path
										d="M2.5 5.5A1.5 1.5 0 0 1 4 4h4.2l1.8 2H16a1.5 1.5 0 0 1 1.5 1.5v7A1.5 1.5 0 0 1 16 16H4a1.5 1.5 0 0 1-1.5-1.5v-9Z"
										stroke-linecap="round"
										stroke-linejoin="round"
									/>
								</svg>
								<span>Examinar</span>
							</button>
						</div>
						<div v-if="installLocationModal.customFolder.trim()" class="install-final-path-alert">
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
									{{ installLocationModal.customFolder.trim().replace(/[\\/]$/, '') }}\{{ installLocationModal.server.title }}
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

	<!-- 3. Zoomed Screenshot Modal with Prev/Next Navigation, Wheel Zoom & Drag Pan -->
	<div
		v-if="zoomedScreenshot"
		class="image-zoom-modal-backdrop"
		role="dialog"
		aria-modal="true"
		aria-label="Captura ampliada"
		@click="closeZoomedScreenshot"
	>
		<div class="image-zoom-modal-container" @click.stop>
			<div class="image-zoom-toolbar">
				<span class="image-zoom-title">
					{{ selectedServer?.title }} - {{ activeScreenshotIndex + 1 }} / {{ previewImages.length }}
				</span>

				<div class="image-zoom-toolbar-actions">
					<button
						type="button"
						class="image-zoom-tool-btn"
						title="Alejar"
						:disabled="zoomScale <= 1"
						@click="adjustZoom(-0.25)"
					>
						-
					</button>
					<span class="image-zoom-scale-pill">{{ Math.round(zoomScale * 100) }}%</span>
					<button
						type="button"
						class="image-zoom-tool-btn"
						title="Acercar"
						:disabled="zoomScale >= 4"
						@click="adjustZoom(0.25)"
					>
						+
					</button>
					<button
						type="button"
						class="image-zoom-tool-btn"
						title="Restablecer vista"
						:disabled="zoomScale === 1 && zoomPanX === 0 && zoomPanY === 0"
						@click="resetZoomView"
					>
						Restablecer
					</button>
					<button
						type="button"
						class="image-zoom-close-btn"
						aria-label="Cerrar vista ampliada"
						@click="closeZoomedScreenshot"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true">
							<path d="M5 5l10 10M15 5 5 15" />
						</svg>
					</button>
				</div>
			</div>

			<div
				class="image-zoom-content"
				:style="{
					cursor: zoomScale > 1 ? (isDraggingZoom ? 'grabbing' : 'grab') : 'zoom-in',
				}"
				@wheel.prevent="handleZoomWheel"
				@mousedown.prevent="handleZoomMouseDown"
				@mousemove="handleZoomMouseMove"
				@mouseup="handleZoomMouseUp"
				@mouseleave="handleZoomMouseUp"
				@dblclick="zoomScale > 1 ? resetZoomView() : adjustZoom(1)"
			>
				<button
					v-if="previewImages.length > 1"
					type="button"
					class="image-zoom-nav-btn prev"
					aria-label="Captura anterior"
					@click.stop="navigateZoomedScreenshot(-1)"
					@mousedown.stop
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M12.5 15l-5-5 5-5" />
					</svg>
				</button>

				<img
					:src="zoomedScreenshot"
					:alt="selectedServer?.title || 'Captura ampliada'"
					class="image-zoom-full"
					draggable="false"
					:style="{
						transform: `translate(${zoomPanX}px, ${zoomPanY}px) scale(${zoomScale})`,
						transition: isDraggingZoom ? 'none' : 'transform 120ms ease-out',
					}"
				/>

				<button
					v-if="previewImages.length > 1"
					type="button"
					class="image-zoom-nav-btn next"
					aria-label="Siguiente captura"
					@click.stop="navigateZoomedScreenshot(1)"
					@mousedown.stop
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M7.5 5l5 5-5 5" />
					</svg>
				</button>
			</div>
		</div>
	</div>

	<!-- First-Launch Account Selection Modal -->
	<div
		v-if="showFirstLaunchAccountModal"
		class="confirm-modal-backdrop"
		role="dialog"
		aria-modal="true"
		aria-labelledby="first-launch-title"
	>
		<div class="confirm-modal-box first-launch-modal-box" @click.stop>
			<div class="confirm-modal-header">
				<div
					class="confirm-modal-icon-wrap"
					style="background: rgba(47, 101, 246, 0.15); border-color: rgba(47, 101, 246, 0.3); color: #3b82f6;"
				>
					<svg
						viewBox="0 0 24 24"
						width="22"
						height="22"
						stroke="currentColor"
						stroke-width="2"
						fill="none"
						stroke-linecap="round"
						stroke-linejoin="round"
					>
						<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
						<circle cx="12" cy="7" r="4" />
					</svg>
				</div>
				<div class="confirm-modal-title-group">
					<h3 id="first-launch-title">¡Bienvenido a Dr3sh App!</h3>
					<p class="confirm-modal-target">Selecciona cómo deseas identificarte para jugar</p>
				</div>
			</div>

			<div v-if="firstLaunchAccountMode === 'choose'" class="first-launch-options">
				<!-- Option 1: Microsoft -->
				<button
					type="button"
					class="first-launch-choice-card"
					@click="handleFirstLaunchChooseMicrosoft"
				>
					<div class="choice-card-icon ms-icon">
						<svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor">
							<path d="M1 1h10v10H1V1zm12 0h10v10H13V1zM1 13h10v10H1V13zm12 0h10v10H13V13z" />
						</svg>
					</div>
					<div class="choice-card-text">
						<span class="choice-card-title">Cuenta de Microsoft</span>
						<span class="choice-card-desc">Inicia sesión con tu cuenta oficial para sincronizar tu skin y capa original.</span>
					</div>
					<span class="choice-card-arrow">→</span>
				</button>

				<!-- Option 2: Offline -->
				<button
					type="button"
					class="first-launch-choice-card"
					@click="handleFirstLaunchChooseOffline"
				>
					<div class="choice-card-icon offline-icon">
						<svg
							viewBox="0 0 24 24"
							width="24"
							height="24"
							stroke="currentColor"
							stroke-width="2"
							fill="none"
							stroke-linecap="round"
							stroke-linejoin="round"
						>
							<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
							<circle cx="12" cy="7" r="4" />
						</svg>
					</div>
					<div class="choice-card-text">
						<span class="choice-card-title">Cuenta sin conexión (Offline)</span>
						<span class="choice-card-desc">Juega de inmediato indicando tu nombre o apodo personalizado.</span>
					</div>
					<span class="choice-card-arrow">→</span>
				</button>
			</div>

			<div v-else class="first-launch-offline-step">
				<label class="first-launch-field-label" for="first-launch-nickname">Nombre de jugador:</label>
				<input
					id="first-launch-nickname"
					v-model="firstLaunchOfflineNickname"
					type="text"
					class="first-launch-input"
					placeholder="Player"
					maxlength="16"
					autofocus
					@keydown.enter.prevent="handleFirstLaunchSubmitOffline"
				/>
				<div class="confirm-modal-actions" style="margin-top: 14px;">
					<button
						type="button"
						class="confirm-modal-cancel-btn"
						@click="firstLaunchAccountMode = 'choose'"
					>
						Atrás
					</button>
					<button
						type="button"
						class="first-launch-primary-btn"
						:disabled="!firstLaunchOfflineNickname.trim()"
						@click="handleFirstLaunchSubmitOffline"
					>
						Continuar
					</button>
				</div>
			</div>

			<div v-if="firstLaunchAccountMode === 'choose'" class="first-launch-footer">
				<button
					type="button"
					class="first-launch-skip-btn"
					@click="handleFirstLaunchClose"
				>
					Jugar como "Player" por ahora
				</button>
			</div>
		</div>
	</div>

	<div
		v-if="diagnosticsModalError"
		class="confirm-modal-backdrop"
		role="dialog"
		aria-modal="true"
		@click="diagnosticsModalError = null"
	>
		<div class="confirm-modal-box" style="max-width: 560px; width: 92%" @click.stop>
			<div class="confirm-modal-header">
				<div class="confirm-modal-icon-wrap" aria-hidden="true">
					<svg viewBox="0 0 24 24">
						<path
							d="M12 9v4m0 4h.01M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"
						/>
					</svg>
				</div>
				<div>
					<h3>No se pudo completar la operación</h3>
					<p class="confirm-modal-target">
						Código: {{ diagnosticsModalError.code }} • Op: {{ diagnosticsModalError.op_id }}
					</p>
				</div>
			</div>

			<div class="confirm-modal-body">
				<p>{{ diagnosticsModalError.message }}</p>

				<div
					v-if="diagnosticsDetailsOpen"
					style="
						margin-top: 12px;
						padding: 10px;
						border-radius: 10px;
						background: rgba(0, 0, 0, 0.35);
						border: 1px solid rgba(255, 255, 255, 0.08);
						max-height: 220px;
						overflow: auto;
						font-family: monospace;
						font-size: 11px;
						white-space: pre-wrap;
						word-break: break-all;
					"
				>
					{{ loadingDiagnostics ? 'Cargando diagnóstico...' : diagnosticsText || diagnosticsModalError.details }}
				</div>
			</div>

			<div
				class="confirm-modal-actions"
				style="display: flex; flex-wrap: wrap; gap: 8px; justify-content: flex-end"
			>
				<button
					type="button"
					class="confirm-modal-cancel-btn"
					@click="handleViewErrorDetails"
				>
					{{ diagnosticsDetailsOpen ? 'Ocultar detalles' : 'Ver detalles' }}
				</button>
				<button
					type="button"
					class="confirm-modal-cancel-btn"
					@click="handleCopyDiagnostics"
				>
					Copiar diagnóstico
				</button>
				<button
					type="button"
					class="confirm-modal-cancel-btn"
					:disabled="sendingSupportReport"
					@click="
						handleSendSupportReport(
							'download_error',
							diagnosticsModalError,
							diagnosticsText || diagnosticsModalError?.details || '',
						)
					"
				>
					{{ sendingSupportReport ? 'Enviando...' : 'Enviar a soporte' }}
				</button>
				<button
					type="button"
					class="confirm-modal-cancel-btn"
					@click="handleOpenLogsFolder"
				>
					Abrir carpeta de logs
				</button>
				<button
					type="button"
					class="install-modal-confirm-btn"
					@click="diagnosticsModalError = null"
				>
					Cerrar
				</button>
			</div>
		</div>
	</div>

	<!-- Console & Logs / Crash Viewer Modal -->
	<div
		v-if="logsModalOpen"
		class="confirm-modal-backdrop"
		role="dialog"
		aria-modal="true"
		aria-label="Consola y logs de la instancia"
		@click="closeInstanceLogsModal"
	>
		<div class="logs-modal-box" @click.stop>
			<div class="logs-modal-header">
				<div class="logs-modal-title-row">
					<div class="logs-modal-icon" aria-hidden="true">
						<svg
							viewBox="0 0 24 24"
							width="20"
							height="20"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
						>
							<polyline points="4 17 10 11 4 5" />
							<line x1="12" y1="19" x2="20" y2="19" />
						</svg>
					</div>
					<div class="logs-modal-title-group">
						<h3>
							<span>Consola y Logs — {{ logsModalInstanceName || 'Instancia' }}</span>
							<span class="logs-status-pill" :class="{ 'is-running': isSelectedRunning }">
								<span class="logs-status-dot" />
								<span>{{ isSelectedRunning ? 'En ejecución' : 'Detenido' }}</span>
							</span>
						</h3>
						<p>
							{{
								logSelectedSource === 'live'
									? 'Consola en vivo (launcher_log.txt)'
									: logSelectedSource.replace(/^.*?::/, '')
							}}
						</p>
					</div>
				</div>

				<button
					type="button"
					class="logs-close-btn"
					aria-label="Cerrar consola"
					@click="closeInstanceLogsModal"
				>
					<svg
						viewBox="0 0 20 20"
						width="16"
						height="16"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
					>
						<path d="M5 5l10 10M15 5 5 15" />
					</svg>
				</button>
			</div>

			<div class="logs-controls-bar">
				<select
					v-model="logSelectedSource"
					class="logs-select"
					aria-label="Seleccionar fuente de log"
					@change="loadSelectedLogSource(logSelectedSource)"
				>
					<option value="live">Consola en vivo (launcher_log.txt)</option>
					<option
						v-for="entry in filteredSelectorEntries"
						:key="`${entry.log_type}::${entry.filename}`"
						:value="`${entry.log_type}::${entry.filename}`"
					>
						{{ entry.log_type === 'CrashReport' ? `[CRASH] ${entry.filename}` : entry.filename }}
					</option>
				</select>

				<input
					v-model="logSearchQuery"
					type="search"
					class="logs-search-input"
					placeholder="Buscar en el log (error, mod, clase...)"
				/>

				<div class="logs-filter-pills" role="group" aria-label="Filtrar líneas">
					<button
						type="button"
						class="logs-filter-pill"
						:class="{ active: logFilterMode === 'all' }"
						@click="handleLogFilterModeChange('all')"
					>
						Todo
					</button>
					<button
						type="button"
						class="logs-filter-pill"
						:class="{ active: logFilterMode === 'errors' }"
						@click="handleLogFilterModeChange('errors')"
					>
						Errores / Warn
					</button>
					<button
						type="button"
						class="logs-filter-pill has-crash"
						:class="{ active: logFilterMode === 'crashes' }"
						@click="handleLogFilterModeChange('crashes')"
					>
						Crash Reports ({{ crashReportEntries.length }})
					</button>
				</div>
			</div>

			<div v-if="detectedCrashHint" class="logs-crash-banner" role="alert">
				<svg
					viewBox="0 0 24 24"
					width="20"
					height="20"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
					style="flex-shrink: 0; margin-top: 2px"
				>
					<path d="M12 9v4m0 4h.01M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
				</svg>
				<div>
					<strong>{{ detectedCrashHint.title }}</strong>
					<span>{{ detectedCrashHint.advice }}</span>
				</div>
			</div>

			<div ref="logOutputEl" class="logs-terminal-box" @scroll="handleLogsScroll">
				<div v-if="logsLoading" class="logs-terminal-empty">
					Cargando registros de la instancia...
				</div>
				<div v-else-if="logStatsAndLines.lines.length === 0" class="logs-terminal-empty">
					{{
						logSearchQuery.trim() || logFilterMode === 'errors'
							? 'No hay líneas que coincidan con el filtro aplicado.'
							: 'Todavía no hay salida registrada para esta fuente.'
					}}
				</div>
				<template v-else>
					<div
						v-if="logStatsAndLines.truncated"
						class="logs-terminal-line is-warn"
						style="margin-bottom: 8px; opacity: 0.85"
					>
						[Mostrando las últimas 2500 líneas de {{ logStatsAndLines.totalMatched }} para mantener el rendimiento]
					</div>
					<div
						v-for="(line, idx) in logStatsAndLines.lines"
						:key="idx"
						class="logs-terminal-line"
						:class="{
							'is-error': line.level === 'error',
							'is-warn': line.level === 'warn',
						}"
					>
						{{ line.text || '\u00A0' }}
					</div>
				</template>
			</div>

			<div class="logs-modal-footer">
				<span class="logs-footer-meta">
					{{ logStatsAndLines.totalMatched }} líneas
					<template v-if="logSelectedSource === 'live'">
						• {{ logAutoScroll ? 'Autodesplazamiento activo' : 'Desplazamiento manual' }}
					</template>
				</span>

				<div class="logs-footer-actions">
					<button
						type="button"
						class="confirm-modal-cancel-btn"
						@click="handleCopyCurrentLog"
					>
						Copiar log
					</button>
					<button
						type="button"
						class="confirm-modal-cancel-btn"
						@click="handleCopySupportReport"
					>
						Reporte soporte
					</button>
					<button
						type="button"
						class="confirm-modal-cancel-btn"
						:disabled="sendingSupportReport"
						@click="handleSendSupportReport(detectedCrashHint ? 'crash' : 'manual')"
					>
						{{ sendingSupportReport ? 'Enviando...' : 'Enviar a soporte' }}
					</button>
					<button
						v-if="logSelectedSource === 'live'"
						type="button"
						class="confirm-modal-cancel-btn"
						@click="handleClearLiveConsole"
					>
						Limpiar vista
					</button>
					<button
						type="button"
						class="confirm-modal-cancel-btn"
						@click="handleOpenCurrentLogsFolder"
					>
						Abrir carpeta
					</button>
					<button
						type="button"
						class="install-modal-confirm-btn"
						@click="closeInstanceLogsModal"
					>
						Cerrar
					</button>
				</div>
			</div>
		</div>
	</div>
</template>
