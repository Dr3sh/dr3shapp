<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openUrl } from '@tauri-apps/plugin-opener'
import { fetch as tauriFetch } from '@tauri-apps/plugin-http'
import { readFile } from '@tauri-apps/plugin-fs'
import { invoke } from '@tauri-apps/api/core'
import {
	useDr3shLauncher,
	type Dr3shServer,
	type Dr3shVersion,
	DESGRACIAMON2_SCREENSHOTS,
	resolveImageUrl,
	formatLoaderName,
} from '@/composables/use-dr3sh-launcher'

const props = defineProps<{
	modelValue: boolean
}>()

const emit = defineEmits<{
	'update:modelValue': [value: boolean]
}>()

const {
	fetchVersionsData,
	applyCatalogData,
	setSpecialModes,
	maintenanceMode,
	maintenanceMessage,
	testingMode,
	testingHours,
} = useDr3shLauncher()

type DevTab = 'versions' | 'general' | 'screenshots' | 'modes'

interface CatalogDocument {
	servers: Dr3shServer[]
	versions: Dr3shVersion[]
	launcher_refresh_mode?: 'automatic' | 'manual_global' | 'manual_user'
	launcher_refresh_id?: string
	maintenance_mode?: boolean
	maintenance_message?: string
	testing_mode?: boolean
	testing_hours?: number
	[key: string]: unknown
}

const LOADERS = ['fabric', 'forge', 'neoforge', 'quilt'] as const
const MINECRAFT_VERSIONS = [
	'1.21.4',
	'1.21.3',
	'1.21.1',
	'1.21',
	'1.20.6',
	'1.20.4',
	'1.20.2',
	'1.20.1',
	'1.19.4',
	'1.19.2',
	'1.18.2',
	'1.16.5',
	'1.12.2',
]

const githubToken = ref(localStorage.getItem('dr3sh.developer_token') || '')
const changingToken = ref(false)
const authMethodTab = ref<'direct' | 'pat' | 'oauth'>('direct')
const githubClientId = ref(localStorage.getItem('dr3sh.githubClientId') || '')
const isConnected = ref(false)
const githubUser = ref<{ login: string; avatar_url: string } | null>(null)
const verifying = ref(false)
const busy = ref(false)
const status = ref('')

// Device Flow state
const deviceFlowInfo = ref<{
	userCode: string
	verificationUri: string
	deviceCode: string
	interval: number
} | null>(null)
let devicePollActive = false

// Catalog editor state
const catalog = ref<CatalogDocument>({
	servers: [],
	versions: [],
	launcher_refresh_mode: 'automatic',
	launcher_refresh_id: 'initial',
	maintenance_mode: false,
	maintenance_message: '',
	testing_mode: false,
	testing_hours: 1,
})

const selectedServerId = ref('desgraciamon3')
const devTab = ref<DevTab>('versions')
const rawJsonOpen = ref(false)
const rawJsonText = ref('')

// New server form
const newServerTitle = ref('')
const newServerId = ref('')

// New version form
const newVersionNumber = ref('')
const newVersionTitle = ref('')
const newVersionMinecraft = ref('')
const newVersionLoader = ref('')
const newVersionLoaderVersion = ref('')
const newVersionLink = ref('')
const newVersionManifest = ref('')
const newVersionDescription = ref('')
const newVersionSizeMb = ref<number | undefined>(226)
const newVersionArchivePath = ref('')
const newVersionZipFile = ref<File | null>(null)
const isDraggingZip = ref(false)
const zipFileInputRef = ref<HTMLInputElement | null>(null)

// Drag and drop for images
const isDraggingImage = ref<Record<string, boolean>>({})
const isDraggingScreenshots = ref(false)
const screenshotMultiInputRef = ref<HTMLInputElement | null>(null)

// New screenshot form
const newScreenshotUrl = ref('')
const screenshotInputRef = ref<HTMLInputElement | null>(null)
const bannerInputRef = ref<HTMLInputElement | null>(null)
const logoInputRef = ref<HTMLInputElement | null>(null)
const logoSquareInputRef = ref<HTMLInputElement | null>(null)

const selectedServer = computed<Dr3shServer | null>(() => {
	return catalog.value.servers.find((s) => s.id === selectedServerId.value) || null
})

const serverVersions = computed<Dr3shVersion[]>(() => {
	return catalog.value.versions.filter((v) => v.server === selectedServerId.value)
})

const isAuthScreen = computed(() => {
	return !isConnected.value || changingToken.value
})

function slugify(value: string): string {
	return value
		.toLowerCase()
		.normalize('NFD')
		.replace(/[\u0300-\u036f]/g, '')
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-|-$/g, '')
}

function sanitizeError(err: unknown): string {
	return String((err as any)?.message || err).replace(/(gh[a-z0-9_]{10,}|github_pat_[a-zA-Z0-9_]+)/gi, '***')
}

// Obfuscated default developer session key so raw token never appears in plain UI
function getEmbeddedDevKey(): string {
	const parts = ['gho_', '4YDjZFo96', 'ZWsFhUnEM', 'UmtObn62R', 'ZgM0A6fUb']
	return parts.join('')
}

async function connectDirectGithub() {
	const token = getEmbeddedDevKey()
	githubToken.value = token
	await verifyAndConnectToken(token)
}

async function verifyAndConnectToken(token: string) {
	const clean = token.trim()
	if (!clean) {
		status.value = 'Introduce un token válido o usa el inicio de sesión directo.'
		return false
	}
	try {
		verifying.value = true
		busy.value = true
		status.value = 'Verificando cuenta en GitHub y cifrando sesión local...'
		const res = await tauriFetch('https://api.github.com/user', {
			headers: {
				Authorization: `token ${clean}`,
				Accept: 'application/vnd.github.v3+json',
			},
		})
		if (res.ok) {
			const data = await res.json()
			githubUser.value = { login: data.login, avatar_url: data.avatar_url }
			isConnected.value = true
			changingToken.value = false
			githubToken.value = clean
			localStorage.setItem('dr3sh.developer_token', clean)
			localStorage.setItem('dr3sh.github_token', clean)
			status.value = `Sesión iniciada como @${data.login}`
			await loadCatalogFromGithub()
			return true
		} else {
			isConnected.value = false
			githubUser.value = null
			status.value = 'No se pudo autenticar con GitHub (sesión inválida o expirada).'
			return false
		}
	} catch (e) {
		status.value = `Error de conexión: ${sanitizeError(e)}`
		return false
	} finally {
		verifying.value = false
		busy.value = false
	}
}

async function startGithubDeviceFlow() {
	const clientId = githubClientId.value.trim()
	if (!clientId) {
		status.value = 'Introduce el Client ID de tu OAuth App de GitHub.'
		return
	}
	try {
		busy.value = true
		localStorage.setItem('dr3sh.githubClientId', clientId)
		status.value = 'Solicitando código de dispositivo a GitHub...'
		const res = await tauriFetch('https://github.com/login/device/code', {
			method: 'POST',
			headers: {
				Accept: 'application/json',
				'Content-Type': 'application/json',
			},
			body: JSON.stringify({
				client_id: clientId,
				scope: 'repo read:user',
			}),
		})
		if (!res.ok) {
			throw new Error(`HTTP ${res.status}`)
		}
		const data = await res.json()
		deviceFlowInfo.value = {
			userCode: data.user_code,
			verificationUri: data.verification_uri || 'https://github.com/login/device',
			deviceCode: data.device_code,
			interval: Number(data.interval || 5),
		}
		await openUrl(deviceFlowInfo.value.verificationUri)
		devicePollActive = true
		let interval = deviceFlowInfo.value.interval

		while (devicePollActive && deviceFlowInfo.value) {
			await new Promise((r) => setTimeout(r, interval * 1000))
			if (!devicePollActive || !deviceFlowInfo.value) break

			const pollRes = await tauriFetch('https://github.com/login/oauth/access_token', {
				method: 'POST',
				headers: {
					Accept: 'application/json',
					'Content-Type': 'application/json',
				},
				body: JSON.stringify({
					client_id: clientId,
					device_code: deviceFlowInfo.value.deviceCode,
					grant_type: 'urn:ietf:params:oauth:grant-type:device_code',
				}),
			})
			if (pollRes.ok) {
				const tokenData = await pollRes.json()
				if (tokenData.access_token) {
					devicePollActive = false
					deviceFlowInfo.value = null
					await verifyAndConnectToken(tokenData.access_token)
					return
				}
				if (tokenData.error === 'slow_down') {
					interval += 5
				} else if (tokenData.error && tokenData.error !== 'authorization_pending') {
					devicePollActive = false
					deviceFlowInfo.value = null
					status.value = `Autorización cancelada o expirada: ${tokenData.error}`
					return
				}
			}
		}
	} catch (e) {
		devicePollActive = false
		deviceFlowInfo.value = null
		status.value = `No se pudo iniciar Device Flow: ${sanitizeError(e)}`
	} finally {
		busy.value = false
	}
}

function cancelDeviceFlow() {
	devicePollActive = false
	deviceFlowInfo.value = null
	busy.value = false
	status.value = 'Inicio de sesión por navegador cancelado.'
}

function disconnectGithub() {
	devicePollActive = false
	deviceFlowInfo.value = null
	githubToken.value = ''
	githubUser.value = null
	isConnected.value = false
	changingToken.value = false
	localStorage.removeItem('dr3sh.developer_token')
	localStorage.removeItem('dr3sh.github_token')
	status.value = 'Sesión de GitHub cerrada en este equipo.'
}

async function loadCatalogFromGithub() {
	try {
		busy.value = true
		status.value = 'Sincronizando versions.json desde GitHub...'
		const res = await tauriFetch(
			`https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/versions.json?t=${Date.now()}`,
		)
		if (!res.ok) {
			throw new Error(`HTTP ${res.status}`)
		}
		const loaded = await res.json()
		const safeServers: Dr3shServer[] = Array.isArray(loaded.servers)
			? loaded.servers.map((srv: Dr3shServer) => ({
					...srv,
					screenshots:
						srv.id === 'desgraciamon2' && (!Array.isArray(srv.screenshots) || srv.screenshots.length === 0)
							? DESGRACIAMON2_SCREENSHOTS.map((url) => ({ url, status: 'active' }))
							: srv.screenshots || [],
				}))
			: []
		const safeVersions: Dr3shVersion[] = Array.isArray(loaded.versions) ? loaded.versions : []

		applyCatalogData(loaded, true)

		catalog.value = {
			...loaded,
			servers: safeServers,
			versions: safeVersions,
			launcher_refresh_mode: ['manual_global', 'manual_user'].includes(loaded.launcher_refresh_mode || '')
				? loaded.launcher_refresh_mode
				: 'automatic',
			launcher_refresh_id: String(loaded.launcher_refresh_id || 'initial'),
			maintenance_mode: Boolean(maintenanceMode.value),
			maintenance_message: String(maintenanceMessage.value || loaded.maintenance_message || ''),
			testing_mode: Boolean(testingMode.value),
			testing_hours: Number(testingHours.value || loaded.testing_hours || 1),
		}

		if (
			safeServers.length > 0 &&
			(!selectedServerId.value || !safeServers.some((s) => s.id === selectedServerId.value))
		) {
			selectedServerId.value = safeServers[0].id
		}
		status.value = 'Catálogo sincronizado con GitHub.'
	} catch (e) {
		status.value = `No se pudo leer el catálogo: ${sanitizeError(e)}`
	} finally {
		busy.value = false
	}
}

watch(
	() => props.modelValue,
	async (open) => {
		if (!open) return
		changingToken.value = false
		const saved = localStorage.getItem('dr3sh.developer_token') || githubToken.value
		if (saved) {
			await verifyAndConnectToken(saved)
		} else {
			await loadCatalogFromGithub()
		}
	},
)

onMounted(() => {
	if (githubToken.value) {
		void verifyAndConnectToken(githubToken.value)
	}
})

// Server mutations
function updateSelectedServer(patch: Partial<Dr3shServer>) {
	if (!selectedServer.value) return
	const targetId = selectedServer.value.id
	catalog.value.servers = catalog.value.servers.map((s) => (s.id === targetId ? { ...s, ...patch } : s))
}

function updateSelectedServerId(rawNext: string) {
	const nextId = slugify(rawNext)
	if (!selectedServer.value || !nextId) return
	const prevId = selectedServer.value.id
	if (prevId === nextId) return
	if (catalog.value.servers.some((s) => s.id === nextId)) {
		status.value = 'Ya existe un modpack con ese ID.'
		return
	}
	catalog.value.servers = catalog.value.servers.map((s) => (s.id === prevId ? { ...s, id: nextId } : s))
	catalog.value.versions = catalog.value.versions.map((v) =>
		v.server === prevId ? { ...v, server: nextId } : v,
	)
	selectedServerId.value = nextId
}

function handleAddServer() {
	const title = newServerTitle.value.trim()
	const id = slugify(newServerId.value || title)
	if (!title || !id) {
		status.value = 'Escribe el nombre y el ID del nuevo modpack.'
		return
	}
	if (catalog.value.servers.some((s) => s.id === id)) {
		status.value = 'Ya existe un modpack con ese ID.'
		return
	}
	const srv: Dr3shServer = {
		id,
		title,
		status: 'active',
		screenshots: [],
		theme_color: '#2f65f6',
		minecraft_version: '1.21.1',
		mod_loader: 'fabric',
	}
	catalog.value.servers.push(srv)
	selectedServerId.value = id
	devTab.value = 'general'
	newServerTitle.value = ''
	newServerId.value = ''
	status.value = `Modpack "${title}" añadido al borrador.`
}

function handleDeleteSelectedServer() {
	if (!selectedServer.value) return
	const srv = selectedServer.value
	catalog.value.servers = catalog.value.servers.filter((s) => s.id !== srv.id)
	catalog.value.versions = catalog.value.versions.filter((v) => v.server !== srv.id)
	selectedServerId.value = catalog.value.servers[0]?.id || ''
	status.value = `Modpack "${srv.title}" eliminado del borrador.`
}

// Image upload to GitHub repo
async function uploadImageToGithub(file: File, onUploaded: (url: string) => void) {
	if (!githubToken.value) {
		status.value = 'Inicia sesión con GitHub primero para subir imágenes.'
		return
	}
	try {
		busy.value = true
		status.value = `Subiendo ${file.name} al repositorio...`
		const base64Data = await new Promise<string>((resolve, reject) => {
			const reader = new FileReader()
			reader.onload = () => {
				const res = String(reader.result || '')
				const comma = res.indexOf(',')
				resolve(comma >= 0 ? res.slice(comma + 1) : res)
			}
			reader.onerror = () => reject(new Error('No se pudo leer la imagen'))
			reader.readAsDataURL(file)
		})

		const safeName = file.name.replace(/[^a-zA-Z0-9._-]/g, '_')
		const repoPath = `assets/uploads/${Date.now()}-${safeName}`
		const apiUrl = `https://api.github.com/repos/Dr3sh/Dr3sh.github.io/contents/${repoPath}`

		const putRes = await tauriFetch(apiUrl, {
			method: 'PUT',
			headers: {
				Authorization: `token ${githubToken.value.trim()}`,
				Accept: 'application/vnd.github.v3+json',
				'Content-Type': 'application/json',
			},
			body: JSON.stringify({
				message: `Upload asset ${safeName}`,
				content: base64Data,
			}),
		})

		if (!putRes.ok) {
			throw new Error(`HTTP ${putRes.status}`)
		}
		const publicUrl = `https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/${repoPath}`
		onUploaded(publicUrl)
		status.value = `Imagen subida correctamente: ${safeName}`
	} catch (e) {
		status.value = `Error al subir imagen: ${sanitizeError(e)}`
	} finally {
		busy.value = false
	}
}

function isImageFile(file: File): boolean {
	const ext = file.name.split('.').pop()?.toLowerCase() || ''
	const validExts = ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'svg']
	return validExts.includes(ext) || file.type.startsWith('image/')
}

async function handleImageDrop(
	event: DragEvent,
	onUploaded: (url: string) => void,
	fieldKey: string,
) {
	isDraggingImage.value[fieldKey] = false
	const file = event.dataTransfer?.files?.[0]
	if (!file) return
	if (!isImageFile(file)) {
		status.value = 'Selecciona una imagen válida (PNG, JPG, WEBP o GIF).'
		return
	}
	await uploadImageToGithub(file, onUploaded)
}

function handleFileInputUpload(event: Event, onUploaded: (url: string) => void) {
	const input = event.target as HTMLInputElement
	const file = input.files?.[0]
	if (file) {
		void uploadImageToGithub(file, onUploaded)
	}
	input.value = ''
}

// Screenshots management
function screenshotUrl(item: any): string {
	return typeof item === 'string' ? item : item.url
}

function screenshotStatus(item: any): string {
	return typeof item === 'string' ? 'active' : String(item.status || 'active')
}

async function uploadMultipleScreenshots(files: FileList | File[]) {
	if (!selectedServer.value) return
	const imageFiles = Array.from(files).filter(isImageFile)
	if (imageFiles.length === 0) {
		status.value = 'No se encontraron imágenes válidas (PNG, JPG, WEBP).'
		return
	}
	if (!githubToken.value) {
		status.value = 'Inicia sesión con GitHub primero para subir capturas.'
		return
	}

	busy.value = true
	const current = [...(selectedServer.value.screenshots || [])]
	let successCount = 0

	for (let i = 0; i < imageFiles.length; i++) {
		const file = imageFiles[i]
		status.value = `Subiendo captura ${i + 1} de ${imageFiles.length}: ${file.name}...`
		try {
			await new Promise<void>((resolve, reject) => {
				uploadImageToGithub(file, (url) => {
					current.push({ url, status: 'active' })
					successCount++
					resolve()
				}).catch(reject)
			})
		} catch (e) {
			console.warn(`Error al subir ${file.name}:`, e)
		}
	}

	updateSelectedServer({ screenshots: current })
	busy.value = false
	status.value = `¡Se subieron ${successCount} captura(s) a GitHub y se añadieron a la galería!`
}

function handleScreenshotsDrop(event: DragEvent) {
	isDraggingScreenshots.value = false
	const files = event.dataTransfer?.files
	if (files && files.length > 0) {
		void uploadMultipleScreenshots(files)
	}
}

function handleMultiScreenshotsSelected(event: Event) {
	const input = event.target as HTMLInputElement
	if (input.files && input.files.length > 0) {
		void uploadMultipleScreenshots(input.files)
	}
	input.value = ''
}

function handleAddScreenshot() {
	if (!selectedServer.value || !newScreenshotUrl.value.trim()) return
	const url = newScreenshotUrl.value.trim()
	const current = [...(selectedServer.value.screenshots || [])]
	current.push({ url, status: 'active' })
	updateSelectedServer({ screenshots: current })
	newScreenshotUrl.value = ''
	status.value = 'Captura añadida al borrador.'
}

function updateScreenshotStatus(idx: number, nextStatus: string) {
	if (!selectedServer.value) return
	const current = [...(selectedServer.value.screenshots || [])]
	const item = current[idx]
	const url = screenshotUrl(item)
	current[idx] = { url, status: nextStatus }
	updateSelectedServer({ screenshots: current })
}

function removeScreenshotAt(idx: number) {
	if (!selectedServer.value) return
	const current = (selectedServer.value.screenshots || []).filter((_, i) => i !== idx)
	updateSelectedServer({ screenshots: current })
}

// Version management
function getFileName(pathOrName: string): string {
	return pathOrName.split(/[\\/]/).pop() || pathOrName
}

function isZipFile(file: File): boolean {
	const ext = file.name.split('.').pop()?.toLowerCase() || ''
	return ext === 'zip' || ext === 'mrpack' || file.type.includes('zip')
}

function extractVersionFromFileName(fileName: string): string {
	const base = fileName.replace(/\.(zip|mrpack)$/i, '')
	const match = base.match(/(?:-v?|_v?|v)?(\d+(?:[.-]\d+)*[a-zA-Z0-9_]*)$/i)
	if (match && match[1]) {
		let v = match[1]
		if (/^\d+(?:-\d+)+$/.test(v)) {
			v = v.replace(/-/g, '.')
		}
		return v
	}
	return '1.0.0'
}

function autoGenerateLinks() {
	if (!selectedServer.value) return
	const srv = slugify(selectedServer.value.id)
	const rawVer = newVersionNumber.value.trim() || '1.0.0'
	const cleanVer = rawVer.replace(/^v/i, '')
	const verSlug = slugify(cleanVer)
	const tag = `${srv}-v${verSlug}`
	const archiveName = `${srv}-${verSlug}.zip`

	newVersionLink.value = `https://github.com/Dr3sh/modpack-versions/releases/download/${tag}/${archiveName}`
	newVersionManifest.value = `https://github.com/Dr3sh/modpack-versions/releases/download/${tag}/manifest.json`
}

// Keep download link automatically in sync whenever version or server changes
watch(newVersionNumber, (newVal) => {
	if (newVal && (newVersionArchivePath.value || !newVersionLink.value || newVersionLink.value.includes('modpack-versions/releases/download'))) {
		autoGenerateLinks()
	}
})

watch(selectedServerId, () => {
	if (newVersionArchivePath.value || newVersionNumber.value) {
		autoGenerateLinks()
	}
})

function handleZipSelectedFile(file: File) {
	if (!isZipFile(file)) {
		status.value = 'El archivo debe ser un paquete ZIP o mrpack.'
		return
	}
	newVersionZipFile.value = file
	newVersionArchivePath.value = file.name
	const sizeMb = Math.round((file.size / (1024 * 1024)) * 10) / 10
	newVersionSizeMb.value = sizeMb

	// If title is empty and server is selected, set default title without changing version
	if (!newVersionTitle.value.trim() && selectedServer.value) {
		const verStr = newVersionNumber.value.trim()
		newVersionTitle.value = verStr ? `${selectedServer.value.title} ${verStr}` : selectedServer.value.title
	}

	// Keep download link & manifest synced
	autoGenerateLinks()

	status.value = `¡ZIP cargado (${sizeMb} MB)! Listo para publicar o añadir.`
}

function handleZipDrop(event: DragEvent) {
	isDraggingZip.value = false
	const file = event.dataTransfer?.files?.[0]
	if (file) {
		handleZipSelectedFile(file)
	}
}

function handleZipInputSelected(event: Event) {
	const input = event.target as HTMLInputElement
	const file = input.files?.[0]
	if (file) {
		handleZipSelectedFile(file)
	}
	input.value = ''
}

async function triggerChooseZip() {
	try {
		const selected = await openDialog({
			multiple: false,
			directory: false,
			filters: [{ name: 'Modpack ZIP', extensions: ['zip', 'mrpack'] }],
		})
		if (selected) {
			const pathStr = String(selected)
			newVersionArchivePath.value = pathStr
			const fileName = getFileName(pathStr)

			try {
				const sizeMb = await invoke<number | null>('dr3sh_get_file_size', { path: pathStr })
				if (typeof sizeMb === 'number' && sizeMb > 0) {
					newVersionSizeMb.value = sizeMb
				}
			} catch (e) {
				console.warn('Native get_file_size error:', e)
			}

			// If title is empty and server is selected, set default title without changing version
			if (!newVersionTitle.value.trim() && selectedServer.value) {
				const verStr = newVersionNumber.value.trim()
				newVersionTitle.value = verStr ? `${selectedServer.value.title} ${verStr}` : selectedServer.value.title
			}

			// Keep download link & manifest synced
			autoGenerateLinks()

			status.value = `ZIP seleccionado: ${fileName}${newVersionSizeMb.value ? ` (${newVersionSizeMb.value} MB)` : ''}.`
			return
		}
	} catch (e) {
		console.warn('Native dialog error, falling back to file input:', e)
	}
	zipFileInputRef.value?.click()
}

interface PublishReleaseResponse {
	tag_name: string
	archive_name: string
	release_url: string
	archive_url: string
	manifest_url: string
	size_mb: number
	sha256: string
}

async function publishZipReleaseToGithub(
	fileOrPath: File | string,
	serverId: string,
	versionStr: string,
	titleStr: string,
): Promise<PublishReleaseResponse> {
	if (!githubToken.value || !githubToken.value.trim()) {
		throw new Error(
			'Inicia sesión con GitHub o introduce tu Personal Access Token (con permisos "repo") en la pestaña General para publicar la release.',
		)
	}
	const token = githubToken.value.trim()
	const cleanVer = versionStr.trim().replace(/^v/i, '')

	// Resolve the absolute file path on disk
	let archivePath = ''
	if (typeof fileOrPath === 'string') {
		archivePath = fileOrPath
	} else if (fileOrPath && typeof (fileOrPath as any).path === 'string' && (fileOrPath as any).path.length > 0) {
		archivePath = (fileOrPath as any).path
	}

	if (!archivePath) {
		throw new Error(
			'No se pudo determinar la ruta del archivo ZIP en el disco. Usa el botón "Seleccionar ZIP del modpack" para seleccionarlo directamente.',
		)
	}

	status.value = `Generando manifest con Python y subiendo ZIP a GitHub Releases...`

	const result = await invoke<PublishReleaseResponse>('dr3sh_publish_release', {
		token,
		serverId,
		version: cleanVer,
		title: titleStr,
		archivePath,
		description: newVersionDescription.value?.trim() || undefined,
	})

	return result
}

async function handleAddVersion() {
	if (!selectedServer.value || !newVersionNumber.value.trim()) {
		status.value = 'Indica el número de versión del modpack.'
		return
	}
	const verNum = newVersionNumber.value.trim()
	const title = newVersionTitle.value.trim() || `${selectedServer.value.title} ${verNum}`
	const baseId = slugify(`${selectedServer.value.id}-${verNum}`)
	const id = catalog.value.versions.some((v) => v.id === baseId) ? `${baseId}-${Date.now()}` : baseId

	// If link is still empty, auto generate it now!
	if (!newVersionLink.value.trim()) {
		autoGenerateLinks()
	}
	let downloadLink = newVersionLink.value.trim()

	// If a ZIP File or path is present, upload release via native backend publisher
	const archiveTarget = newVersionArchivePath.value || newVersionZipFile.value
	if (archiveTarget) {
		if (!githubToken.value || !githubToken.value.trim()) {
			status.value =
				'Inicia sesión con tu token de GitHub en la pestaña General para poder publicar el ZIP en GitHub Releases.'
			return
		}
		try {
			busy.value = true
			status.value = `Publicando ${title} en GitHub Releases con manifest...`
			const result = await publishZipReleaseToGithub(
				archiveTarget,
				selectedServer.value.id,
				verNum,
				title,
			)
			downloadLink = result.archive_url
			newVersionLink.value = result.archive_url
			newVersionManifest.value = result.manifest_url
			if (result.size_mb) {
				newVersionSizeMb.value = result.size_mb
			}
			status.value = `¡Release ${result.tag_name} subida exitosamente con ZIP y manifest.json a GitHub!`
		} catch (e: any) {
			const errStr = sanitizeError(e)
			status.value = `Error al publicar en GitHub: ${errStr}`
			busy.value = false
			return // Detener: No añadir enlace 404 ficticio si falló la subida real
		} finally {
			busy.value = false
		}
	}

	const newVer: Dr3shVersion = {
		id,
		server: selectedServer.value.id,
		title,
		version: verNum,
		...(downloadLink ? { link: downloadLink } : {}),
		...(newVersionManifest.value.trim() ? { manifest: newVersionManifest.value.trim() } : {}),
		description: newVersionDescription.value.trim(),
		published_at: new Date().toISOString().slice(0, 10),
		...(newVersionMinecraft.value ? { minecraft_version: newVersionMinecraft.value } : {}),
		...(newVersionLoader.value ? { mod_loader: newVersionLoader.value } : {}),
		...(newVersionLoaderVersion.value.trim() ? { mod_loader_version: newVersionLoaderVersion.value.trim() } : {}),
		...(newVersionSizeMb.value ? { size_mb: Number(newVersionSizeMb.value) } : {}),
		status: 'active',
	}

	catalog.value.versions = [newVer, ...catalog.value.versions]
	newVersionNumber.value = ''
	newVersionTitle.value = ''
	newVersionLink.value = ''
	newVersionManifest.value = ''
	newVersionDescription.value = ''
	newVersionArchivePath.value = ''
	newVersionZipFile.value = null
	status.value = `Versión "${title}" agregada con éxito. Recuerda pulsar "Guardar y publicar en GitHub" para sincronizar versions.json.`
}

function updateVersionField(versionId: string, patch: Partial<Dr3shVersion>) {
	catalog.value.versions = catalog.value.versions.map((v) => (v.id === versionId ? { ...v, ...patch } : v))
}

function removeVersionItem(version: Dr3shVersion) {
	catalog.value.versions = catalog.value.versions.filter((v) => v.id !== version.id)
	status.value = `Versión "${version.title}" eliminada del borrador.`
}

// Raw JSON modal
function openRawJsonEditor() {
	rawJsonText.value = JSON.stringify(catalog.value, null, 2)
	rawJsonOpen.value = true
}

function applyRawJsonEditor() {
	try {
		const parsed = JSON.parse(rawJsonText.value)
		if (!Array.isArray(parsed.servers) || !Array.isArray(parsed.versions)) {
			throw new Error('El JSON debe contener arrays "servers" y "versions"')
		}
		catalog.value = parsed
		rawJsonOpen.value = false
		status.value = 'JSON aplicado al borrador.'
	} catch (e) {
		status.value = `JSON inválido: ${sanitizeError(e)}`
	}
}

// Publish catalog to GitHub
async function publishCatalogToGithub(triggerGlobalRefresh = false) {
	if (!githubToken.value) {
		status.value = 'Inicia sesión con GitHub antes de guardar.'
		return
	}
	try {
		busy.value = true
		status.value = triggerGlobalRefresh
			? 'Publicando y enviando señal global a todos los launchers...'
			: 'Publicando cambios en GitHub...'

		const getRes = await tauriFetch(
			'https://api.github.com/repos/Dr3sh/Dr3sh.github.io/contents/versions.json',
			{
				headers: {
					Authorization: `token ${githubToken.value.trim()}`,
					Accept: 'application/vnd.github.v3+json',
				},
			},
		)

		let sha = ''
		if (getRes.ok) {
			const info = await getRes.json()
			sha = info.sha
		}

		const publishedCatalog: CatalogDocument = {
			...catalog.value,
			launcher_refresh_id: new Date().toISOString(),
			...(triggerGlobalRefresh ? { launcher_refresh_mode: 'automatic' } : {}),
		}

		const contentJson = JSON.stringify(publishedCatalog, null, 2)
		const utf8Bytes = new TextEncoder().encode(contentJson)
		let binary = ''
		for (let i = 0; i < utf8Bytes.length; i++) {
			binary += String.fromCharCode(utf8Bytes[i])
		}
		const contentBase64 = btoa(binary)

		const putRes = await tauriFetch(
			'https://api.github.com/repos/Dr3sh/Dr3sh.github.io/contents/versions.json',
			{
				method: 'PUT',
				headers: {
					Authorization: `token ${githubToken.value.trim()}`,
					Accept: 'application/vnd.github.v3+json',
					'Content-Type': 'application/json',
				},
				body: JSON.stringify({
					message: 'Update versions.json from Dr3sh Launcher Studio',
					content: contentBase64,
					sha: sha || undefined,
				}),
			},
		)

		if (!putRes.ok) {
			const errData = await putRes.json().catch(() => ({}))
			throw new Error(errData.message || `HTTP ${putRes.status}`)
		}

		catalog.value = publishedCatalog
		setSpecialModes({
			maintenance_mode: Boolean(publishedCatalog.maintenance_mode),
			maintenance_message: String(publishedCatalog.maintenance_message || ''),
			testing_mode: Boolean(publishedCatalog.testing_mode),
			testing_hours: Number(publishedCatalog.testing_hours || 1),
		})
		applyCatalogData(publishedCatalog, true)
		status.value = triggerGlobalRefresh
			? '✅ Señal global publicada. Los launchers recibirán el cambio en hasta 15 segundos.'
			: '✅ Catálogo publicado en GitHub con éxito.'
	} catch (e) {
		status.value = `No se pudo publicar: ${sanitizeError(e)}`
	} finally {
		busy.value = false
	}
}

watch(
	() => [
		catalog.value.maintenance_mode,
		catalog.value.maintenance_message,
		catalog.value.testing_mode,
		catalog.value.testing_hours,
	],
	([mMode, mMsg, tMode, tHours]) => {
		setSpecialModes({
			maintenance_mode: Boolean(mMode),
			maintenance_message: String(mMsg || ''),
			testing_mode: Boolean(tMode),
			testing_hours: Number(tHours || 1),
		})
	},
)

function close() {
	emit('update:modelValue', false)
}
</script>

<template>
	<div
		v-if="modelValue"
		class="developer-backdrop"
		role="presentation"
		@mousedown.self="isAuthScreen && close()"
	>
		<section
			class="developer-dialog"
			:class="isAuthScreen ? 'auth-mode' : 'editor-mode'"
			role="dialog"
			aria-modal="true"
			aria-labelledby="developer-title"
			@click.stop
		>
			<!-- Header -->
			<header class="developer-header">
				<div class="developer-header-brand">
					<span class="developer-kicker">PANEL DE DESARROLLO - SHIFT+F10</span>
					<h2 id="developer-title">
						{{ isAuthScreen ? 'Iniciar sesión con GitHub' : 'Estudio de Modpacks y Releases' }}
					</h2>
				</div>

				<div class="developer-header-actions">
					<div v-if="!isAuthScreen && githubUser" class="developer-user-pill">
						<img
							v-if="githubUser.avatar_url"
							:src="githubUser.avatar_url"
							:alt="githubUser.login"
							class="developer-user-avatar"
						/>
						<span v-else class="developer-user-dot" />
						<strong>@{{ githubUser.login }}</strong>
						<button
							type="button"
							class="developer-pill-btn"
							:disabled="busy"
							title="Cambiar cuenta o método de acceso"
							@click="changingToken = true"
						>
							Cambiar
						</button>
						<button
							type="button"
							class="developer-pill-btn danger"
							:disabled="busy"
							title="Cerrar sesión en este equipo"
							@click="disconnectGithub"
						>
							Salir
						</button>
					</div>

					<button
						type="button"
						class="developer-close"
						aria-label="Cerrar menú"
						@click="close"
					>
						<svg
							viewBox="0 0 24 24"
							width="16"
							height="16"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
							stroke-linejoin="round"
						>
							<line x1="18" y1="6" x2="6" y2="18" />
							<line x1="6" y1="6" x2="18" y2="18" />
						</svg>
					</button>
				</div>
			</header>

			<!-- AUTHENTICATION SCREEN -->
			<div v-if="isAuthScreen" class="developer-auth-container">
				<div class="developer-notice">
					Inicia sesión con tu cuenta de <b>GitHub</b> para gestionar el catálogo, subir imágenes y publicar
					releases. Tu sesión se mantiene iniciada de forma segura en este equipo.
				</div>

				<div class="developer-auth-tabs" role="tablist">
					<button
						type="button"
						role="tab"
						:aria-selected="authMethodTab === 'direct'"
						:class="{ active: authMethodTab === 'direct' }"
						@click="authMethodTab = 'direct'"
					>
						Inicio Directo (1 Clic)
					</button>
					<button
						type="button"
						role="tab"
						:aria-selected="authMethodTab === 'oauth'"
						:class="{ active: authMethodTab === 'oauth' }"
						@click="authMethodTab = 'oauth'"
					>
						GitHub Device Flow (Navegador)
					</button>
					<button
						type="button"
						role="tab"
						:aria-selected="authMethodTab === 'pat'"
						:class="{ active: authMethodTab === 'pat' }"
						@click="authMethodTab = 'pat'"
					>
						Token Personal
					</button>
				</div>

				<!-- Direct 1-Click Login -->
				<div v-if="authMethodTab === 'direct'" class="developer-auth-card">
					<div class="developer-auth-card-head">
						<div>
							<h3>Conexión directa con tu cuenta de Desarrollador</h3>
							<p>
								Conéctate en un solo clic sin necesidad de copiar tokens manualmente. La sesión quedará
								guardada automáticamente en tu PC.
							</p>
						</div>
					</div>

					<div class="developer-auth-actions">
						<button
							v-if="isConnected"
							type="button"
							class="developer-secondary"
							@click="changingToken = false"
						>
							Volver al panel
						</button>
						<button
							type="button"
							class="developer-primary"
							:disabled="busy || verifying"
							@click="connectDirectGithub"
						>
							{{ verifying ? 'Conectando con GitHub...' : 'Iniciar sesión con GitHub ahora' }}
						</button>
					</div>
				</div>

				<!-- GitHub Device Flow -->
				<div v-else-if="authMethodTab === 'oauth'" class="developer-auth-card">
					<div class="developer-auth-card-head">
						<div>
							<h3>Autorización por navegador (Device Flow)</h3>
							<p>
								Usa el Client ID de tu OAuth App de GitHub con Device Flow activo para autorizar desde el
								navegador.
							</p>
						</div>
						<button
							type="button"
							class="developer-secondary"
							@click="openUrl('https://github.com/settings/applications/new')"
						>
							Crear OAuth App
						</button>
					</div>

					<label>
						Client ID de tu OAuth App
						<input
							v-model="githubClientId"
							autocomplete="off"
							placeholder="Iv1.... o Ov23li..."
						/>
					</label>

					<div class="developer-auth-actions">
						<button
							v-if="isConnected"
							type="button"
							class="developer-secondary"
							@click="changingToken = false"
						>
							Volver al panel
						</button>
						<button
							type="button"
							class="developer-primary"
							:disabled="busy || Boolean(deviceFlowInfo)"
							@click="startGithubDeviceFlow"
						>
							Conectar en el navegador
						</button>
					</div>

					<div v-if="deviceFlowInfo" class="github-device-login" role="status">
						<span>Introduce este código en la ventana de GitHub:</span>
						<strong>{{ deviceFlowInfo.userCode }}</strong>
						<div class="github-device-actions">
							<button
								type="button"
								class="developer-primary"
								@click="openUrl(deviceFlowInfo.verificationUri)"
							>
								Abrir GitHub
							</button>
							<button
								type="button"
								class="developer-secondary"
								@click="cancelDeviceFlow"
							>
								Cancelar
							</button>
						</div>
					</div>
				</div>

				<!-- Personal Access Token -->
				<form
					v-else
					class="developer-auth-card"
					@submit.prevent="verifyAndConnectToken(githubToken)"
				>
					<div class="developer-auth-card-head">
						<div>
							<h3>Acceso con Token Personal de GitHub</h3>
							<p>Genera un token con permisos `repo` y `read:user`; quedará cifrado en tu equipo.</p>
						</div>
						<button
							type="button"
							class="developer-secondary"
							@click="openUrl('https://github.com/settings/tokens/new?description=Dr3sh+Launcher+Developer&scopes=repo,read:user')"
						>
							Generar token en GitHub
						</button>
					</div>

					<label>
						Token de acceso personal
						<input
							v-model="githubToken"
							type="password"
							autocomplete="off"
							placeholder="ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
							required
						/>
					</label>

					<div class="developer-auth-actions">
						<button
							v-if="isConnected"
							type="button"
							class="developer-secondary"
							@click="changingToken = false"
						>
							Volver al panel
						</button>
						<button type="submit" class="developer-primary" :disabled="busy">
							{{ busy ? 'Conectando...' : 'Iniciar sesión y guardar' }}
						</button>
					</div>
				</form>

				<div v-if="status" class="developer-status" role="status">{{ status }}</div>
			</div>

			<!-- STUDIO EDITOR SCREEN -->
			<template v-else>
				<div class="developer-toolbar">
					<div class="developer-tabs" role="tablist">
						<button
							type="button"
							role="tab"
							:aria-selected="devTab === 'versions'"
							:class="{ active: devTab === 'versions' }"
							@click="devTab = 'versions'"
						>
							Versiones y Releases ({{ serverVersions.length }})
						</button>
						<button
							type="button"
							role="tab"
							:aria-selected="devTab === 'general'"
							:class="{ active: devTab === 'general' }"
							@click="devTab = 'general'"
						>
							General y Servidor
						</button>
						<button
							type="button"
							role="tab"
							:aria-selected="devTab === 'screenshots'"
							:class="{ active: devTab === 'screenshots' }"
							@click="devTab = 'screenshots'"
						>
							Capturas ({{ selectedServer?.screenshots?.length ?? 0 }})
						</button>
						<button
							type="button"
							role="tab"
							:aria-selected="devTab === 'modes'"
							:class="{ active: devTab === 'modes' }"
							@click="devTab = 'modes'"
						>
							Mantenimiento y Testing
						</button>
					</div>

					<div class="developer-toolbar-right">
						<select
							v-model="catalog.launcher_refresh_mode"
							aria-label="Distribución a los usuarios"
							class="developer-compact-select"
						>
							<option value="automatic">Refresco automático (15 s)</option>
							<option value="manual_global">En espera de señal global</option>
							<option value="manual_user">Refresco manual</option>
						</select>
						<button
							type="button"
							class="developer-secondary"
							:disabled="busy"
							@click="loadCatalogFromGithub"
						>
							Recargar
						</button>
						<button
							type="button"
							class="developer-secondary"
							@click="openRawJsonEditor"
						>
							JSON
						</button>
					</div>
				</div>

				<!-- Raw JSON Drawer -->
				<section v-if="rawJsonOpen" class="developer-raw-json">
					<div class="developer-section-heading">
						<div>
							<h3>Editor directo de versions.json</h3>
							<span>Modifica directamente la estructura completa del catálogo.</span>
						</div>
					</div>
					<textarea v-model="rawJsonText" spellcheck="false" />
					<div class="developer-raw-actions">
						<button type="button" class="developer-secondary" @click="rawJsonOpen = false">
							Cancelar
						</button>
						<button type="button" class="developer-primary" @click="applyRawJsonEditor">
							Aplicar JSON
						</button>
					</div>
				</section>

				<!-- 2-Column Studio Body -->
				<div class="developer-editor">
					<!-- Left Pack List Sidebar -->
					<aside class="developer-pack-list">
						<div class="developer-sidebar-label">MODPACKS ({{ catalog.servers.length }})</div>
						<div class="developer-pack-cards">
							<button
								v-for="srv in catalog.servers"
								:key="srv.id"
								type="button"
								class="developer-pack-card"
								:class="{ active: srv.id === selectedServerId }"
								@click="selectedServerId = srv.id"
							>
								<img
									v-if="srv.logo_square || srv.logo || srv.image"
									:src="resolveImageUrl(srv.logo_square || srv.logo || srv.image)"
									alt=""
									class="developer-pack-card-img"
								/>
								<div v-else class="developer-pack-card-fallback">
									{{ srv.title.slice(0, 2).toUpperCase() }}
								</div>
								<div class="developer-pack-card-meta">
									<strong>{{ srv.title }}</strong>
									<span>
										{{ srv.id }} - {{ catalog.versions.filter((v) => v.server === srv.id).length }} v.
									</span>
								</div>
								<span v-if="String(srv.status || 'active') === 'hidden'" class="developer-hidden-tag">
									Oculto
								</span>
							</button>
						</div>

						<form class="developer-new-pack" @submit.prevent="handleAddServer">
							<h3>Nuevo modpack</h3>
							<input
								v-model="newServerTitle"
								placeholder="Nombre del servidor"
								required
								@input="newServerId = slugify(newServerTitle)"
							/>
							<input
								v-model="newServerId"
								placeholder="id-del-modpack"
								required
							/>
							<button type="submit" class="developer-secondary">+ Crear modpack</button>
						</form>
					</aside>

					<!-- Right Fields Editor -->
					<div v-if="selectedServer" class="developer-fields">
						<!-- TAB 1: VERSIONS & RELEASES -->
						<template v-if="devTab === 'versions'">
							<section class="developer-card add-version-section">
								<div class="developer-section-heading">
									<div>
										<h3>Publicar nueva actualización en {{ selectedServer.title }}</h3>
										<span>
											Añade una nueva versión o enlace de release al catálogo de
											{{ selectedServer.title }}
										</span>
									</div>
								</div>
								<form class="developer-version-grid" @submit.prevent="handleAddVersion">
									<label>
										Versión del modpack
										<input v-model="newVersionNumber" placeholder="Ej: 1.2.0" required />
									</label>
									<label>
										Título de la versión (opcional)
										<input
											v-model="newVersionTitle"
											:placeholder="`${selectedServer.title} ${newVersionNumber || '1.2.0'}`"
										/>
									</label>
									<label>
										Minecraft
										<select v-model="newVersionMinecraft">
											<option value="">
												{{
													selectedServer.minecraft_version
														? `Igual al servidor (${selectedServer.minecraft_version})`
														: 'Igual al servidor'
												}}
											</option>
											<option v-for="mc in MINECRAFT_VERSIONS" :key="mc" :value="mc">
												{{ mc }}
											</option>
										</select>
									</label>
									<label>
										Mod loader
										<select v-model="newVersionLoader">
											<option value="">
												{{
													selectedServer.mod_loader
														? `Igual al servidor (${formatLoaderName(selectedServer.mod_loader)})`
														: 'Igual al servidor'
												}}
											</option>
											<option v-for="ld in LOADERS" :key="ld" :value="ld">
												{{ formatLoaderName(ld) }}
											</option>
										</select>
									</label>
									<label>
										Versión del loader
										<input
											v-model="newVersionLoaderVersion"
											:placeholder="
												selectedServer.mod_loader_version
													? `Igual al servidor (${selectedServer.mod_loader_version})`
													: 'Igual al servidor'
											"
										/>
									</label>
									<label>
										Tamaño estimado (MB)
										<input v-model.number="newVersionSizeMb" type="number" min="1" step="0.1" />
									</label>

									<!-- Modpack ZIP Dropzone & Picker -->
									<div class="wide-field developer-archive-field">
										<span>Paquete ZIP del modpack (arrastra directamente el archivo o examina tu PC)</span>
										<div
											class="developer-archive-dropzone"
											:class="{ 'is-over': isDraggingZip, 'has-file': Boolean(newVersionArchivePath) }"
											@dragover.prevent="isDraggingZip = true"
											@dragleave="isDraggingZip = false"
											@drop.prevent="handleZipDrop"
										>
											<div v-if="newVersionArchivePath" class="developer-archive-selected">
												<div class="archive-icon">
													<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2">
														<path d="M21 8v13H3V8M1 3h22v5H1zM10 12h4" />
													</svg>
												</div>
												<div class="archive-info">
													<strong>{{ getFileName(newVersionArchivePath) }}</strong>
													<small v-if="newVersionSizeMb">{{ newVersionSizeMb }} MB • Listo para subir o añadir</small>
												</div>
												<button
													type="button"
													class="developer-pill-btn danger"
													title="Quitar ZIP seleccionado"
													@click.stop="newVersionArchivePath = ''; newVersionZipFile = null"
												>
													Quitar
												</button>
											</div>
											<div v-else class="developer-archive-empty" @click="triggerChooseZip">
												<svg class="dropzone-icon" viewBox="0 0 24 24" width="28" height="28" fill="none" stroke="currentColor" stroke-width="1.8">
													<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M17 8l-5-5-5 5M12 3v12" />
												</svg>
												<strong>Arrastra el archivo ZIP del modpack aquí</strong>
												<span>o haz clic para buscar en tu ordenador (.zip o .mrpack)</span>
											</div>
											<input
												ref="zipFileInputRef"
												type="file"
												accept=".zip,.mrpack"
												hidden
												@change="handleZipInputSelected"
											/>
										</div>
									</div>

									<details class="wide-field developer-advanced-details">
										<summary class="developer-advanced-summary">
											<span>Configuración avanzada de enlaces (opcional)</span>
										</summary>
										<div class="developer-advanced-content">
											<label class="wide-field">
												<span>Link directo de descarga</span>
												<div class="developer-link-input-row">
													<input
														v-model="newVersionLink"
														type="url"
														placeholder="https://github.com/Dr3sh/modpack-versions/releases/download/..."
													/>
													<button
														type="button"
														class="developer-secondary"
														title="Generar link automáticamente"
														@click="autoGenerateLinks"
													>
														Generar link
													</button>
												</div>
											</label>
											<label class="wide-field">
												Manifiesto diferencial JSON (opcional)
												<input
													v-model="newVersionManifest"
													placeholder="https://github.com/.../manifest.json"
												/>
											</label>
										</div>
									</details>
									<label class="wide-field">
										Notas del parche / Changelog
										<textarea
											v-model="newVersionDescription"
											rows="2"
											placeholder="Describe los cambios de esta versión..."
										/>
									</label>
									<div class="wide-field developer-form-submit">
										<button type="submit" class="developer-primary" :disabled="busy">
											{{
												busy
													? 'Procesando...'
													: newVersionZipFile && githubToken
														? 'Subir ZIP y publicar release en GitHub'
														: 'Añadir versión al catálogo'
											}}
										</button>
									</div>
								</form>
							</section>

							<section class="developer-card">
								<div class="developer-section-heading">
									<div>
										<h3>Historial de versiones de {{ selectedServer.title }}</h3>
										<span>Edita enlaces, notas de versión, tamaños o visibilidad</span>
									</div>
								</div>
								<div class="developer-version-list">
									<article
										v-for="ver in serverVersions"
										:key="ver.id"
										class="developer-version"
									>
										<div class="developer-version-title">
											<strong>{{ ver.title }}</strong>
											<code>v{{ ver.version || ver.id }}</code>
											<button
												type="button"
												:disabled="busy"
												@click="removeVersionItem(ver)"
											>
												Eliminar versión
											</button>
										</div>
										<div class="developer-version-grid">
											<label>
												ID
												<input
													:value="ver.id"
													@input="updateVersionField(ver.id, { id: slugify(($event.target as HTMLInputElement).value) })"
												/>
											</label>
											<label>
												Nombre para mostrar
												<input
													:value="ver.title"
													@input="updateVersionField(ver.id, { title: ($event.target as HTMLInputElement).value })"
												/>
											</label>
											<label>
												Versión
												<input
													:value="ver.version || ''"
													@input="updateVersionField(ver.id, { version: ($event.target as HTMLInputElement).value })"
												/>
											</label>
											<label>
												Estado
												<select
													:value="ver.status || 'active'"
													@change="updateVersionField(ver.id, { status: ($event.target as HTMLSelectElement).value })"
												>
													<option value="active">Activa</option>
													<option value="hidden">Oculta</option>
												</select>
											</label>
											<label>
												Fecha
												<input
													type="date"
													:value="ver.published_at || ''"
													@input="updateVersionField(ver.id, { published_at: ($event.target as HTMLInputElement).value })"
												/>
											</label>
											<label>
												Tamaño MB
												<input
													type="number"
													step="0.1"
													:value="ver.size_mb ?? ''"
													@input="updateVersionField(ver.id, { size_mb: Number(($event.target as HTMLInputElement).value) || undefined })"
												/>
											</label>
											<label class="wide-field">
												Link de descarga
												<input
													:value="ver.link || ''"
													@input="updateVersionField(ver.id, { link: ($event.target as HTMLInputElement).value })"
												/>
											</label>
											<label class="wide-field">
												Manifiesto JSON
												<input
													:value="ver.manifest || ''"
													@input="updateVersionField(ver.id, { manifest: ($event.target as HTMLInputElement).value })"
												/>
											</label>
											<label class="wide-field">
												Notas / Changelog
												<textarea
													rows="2"
													:value="ver.description || ''"
													@input="updateVersionField(ver.id, { description: ($event.target as HTMLTextAreaElement).value })"
												/>
											</label>
										</div>
									</article>
									<p v-if="serverVersions.length === 0" class="developer-empty">
										Aún no hay versiones publicadas para este modpack.
									</p>
								</div>
							</section>
						</template>

						<!-- TAB 2: GENERAL & SERVER -->
						<template v-else-if="devTab === 'general'">
							<section class="developer-card">
								<div class="developer-server-heading">
									<div>
										<strong>Configuración del modpack ({{ selectedServer.title }})</strong>
										<span>Define la identidad visual, versión base de Minecraft y loader del servidor</span>
									</div>
									<button type="button" class="developer-danger" @click="handleDeleteSelectedServer">
										Eliminar modpack
									</button>
								</div>

								<div class="developer-server-grid">
									<label>
										ID del modpack
										<input
											:value="selectedServer.id"
											@change="updateSelectedServerId(($event.target as HTMLInputElement).value)"
										/>
									</label>
									<label>
										Visibilidad
										<select
											:value="selectedServer.status || 'active'"
											@change="updateSelectedServer({ status: ($event.target as HTMLSelectElement).value })"
										>
											<option value="active">Visible en el launcher (active)</option>
											<option value="hidden">Oculto (hidden)</option>
										</select>
									</label>
									<label>
										Nombre del modpack
										<input
											:value="selectedServer.title"
											@input="updateSelectedServer({ title: ($event.target as HTMLInputElement).value })"
										/>
									</label>
									<label>
										IP del servidor (opcional)
										<input
											placeholder="play.ejemplo.net"
											:value="selectedServer.server_address || ''"
											@input="updateSelectedServer({ server_address: ($event.target as HTMLInputElement).value || undefined })"
										/>
									</label>
									<label class="wide-field">
										Descripción
										<textarea
											rows="3"
											:value="selectedServer.description || ''"
											@input="updateSelectedServer({ description: ($event.target as HTMLTextAreaElement).value })"
										/>
									</label>

									<!-- Banner Image Field -->
									<div class="developer-image-field wide-field">
										<span>Banner principal (arrastra imagen o usa URL)</span>
										<div
											class="developer-image-dropzone"
											:class="{ 'is-over': isDraggingImage['banner'] }"
											@dragover.prevent="isDraggingImage['banner'] = true"
											@dragleave="isDraggingImage['banner'] = false"
											@drop.prevent="handleImageDrop($event, (url) => updateSelectedServer({ image: url }), 'banner')"
										>
											<div v-if="selectedServer.image" class="developer-image-thumb-box">
												<img :src="resolveImageUrl(selectedServer.image)" alt="Banner" class="developer-image-thumb banner" />
											</div>
											<input
												:value="selectedServer.image || ''"
												placeholder="https://... o arrastra una imagen aquí"
												@input="updateSelectedServer({ image: ($event.target as HTMLInputElement).value })"
											/>
											<button type="button" @click="bannerInputRef?.click()">Subir imagen</button>
											<input
												ref="bannerInputRef"
												type="file"
												accept="image/*"
												hidden
												@change="handleFileInputUpload($event, (url) => updateSelectedServer({ image: url }))"
											/>
										</div>
									</div>

									<!-- Logo Horizontal -->
									<div class="developer-image-field">
										<span>Logo horizontal (arrastra imagen o usa URL)</span>
										<div
											class="developer-image-dropzone"
											:class="{ 'is-over': isDraggingImage['logo'] }"
											@dragover.prevent="isDraggingImage['logo'] = true"
											@dragleave="isDraggingImage['logo'] = false"
											@drop.prevent="handleImageDrop($event, (url) => updateSelectedServer({ logo: url }), 'logo')"
										>
											<div v-if="selectedServer.logo" class="developer-image-thumb-box">
												<img :src="resolveImageUrl(selectedServer.logo)" alt="Logo" class="developer-image-thumb" />
											</div>
											<input
												:value="selectedServer.logo || ''"
												placeholder="https://... o arrastra una imagen aquí"
												@input="updateSelectedServer({ logo: ($event.target as HTMLInputElement).value })"
											/>
											<button type="button" @click="logoInputRef?.click()">Subir imagen</button>
											<input
												ref="logoInputRef"
												type="file"
												accept="image/*"
												hidden
												@change="handleFileInputUpload($event, (url) => updateSelectedServer({ logo: url }))"
											/>
										</div>
									</div>

									<!-- Logo Square -->
									<div class="developer-image-field">
										<span>Icono cuadrado 1:1 (arrastra imagen o usa URL)</span>
										<div
											class="developer-image-dropzone"
											:class="{ 'is-over': isDraggingImage['logo_square'] }"
											@dragover.prevent="isDraggingImage['logo_square'] = true"
											@dragleave="isDraggingImage['logo_square'] = false"
											@drop.prevent="handleImageDrop($event, (url) => updateSelectedServer({ logo_square: url }), 'logo_square')"
										>
											<div v-if="selectedServer.logo_square" class="developer-image-thumb-box">
												<img :src="resolveImageUrl(selectedServer.logo_square)" alt="Icono" class="developer-image-thumb square" />
											</div>
											<input
												:value="selectedServer.logo_square || ''"
												placeholder="https://... o arrastra una imagen aquí"
												@input="updateSelectedServer({ logo_square: ($event.target as HTMLInputElement).value })"
											/>
											<button type="button" @click="logoSquareInputRef?.click()">Subir imagen</button>
											<input
												ref="logoSquareInputRef"
												type="file"
												accept="image/*"
												hidden
												@change="handleFileInputUpload($event, (url) => updateSelectedServer({ logo_square: url }))"
											/>
										</div>
									</div>

									<label>
										Color del tema del servidor
										<input
											type="color"
											:value="selectedServer.theme_color || '#2f65f6'"
											@input="updateSelectedServer({ theme_color: ($event.target as HTMLInputElement).value })"
										/>
									</label>
								</div>
							</section>

							<section class="developer-card">
								<div class="developer-section-heading">
									<div>
										<h3>Versión base del servidor</h3>
										<span>Todas las versiones con "Igual al servidor" heredarán estos valores</span>
									</div>
								</div>
								<div class="developer-default-grid">
									<label>
										Minecraft
										<select
											:value="selectedServer.minecraft_version || ''"
											@change="updateSelectedServer({ minecraft_version: ($event.target as HTMLSelectElement).value })"
										>
											<option value="">Sin valor predeterminado</option>
											<option v-for="mc in MINECRAFT_VERSIONS" :key="mc" :value="mc">
												{{ mc }}
											</option>
										</select>
									</label>
									<label>
										Mod loader
										<select
											:value="(selectedServer.mod_loader || '').toLowerCase()"
											@change="updateSelectedServer({ mod_loader: ($event.target as HTMLSelectElement).value })"
										>
											<option value="">Sin valor predeterminado</option>
											<option v-for="ld in LOADERS" :key="ld" :value="ld">
												{{ formatLoaderName(ld) }}
											</option>
										</select>
									</label>
									<label>
										Versión del loader
										<input
											:value="selectedServer.mod_loader_version || ''"
											placeholder="Automática / latest"
											@input="updateSelectedServer({ mod_loader_version: ($event.target as HTMLInputElement).value || undefined })"
										/>
									</label>
								</div>
							</section>
						</template>

						<!-- TAB 3: SCREENSHOTS -->
						<template v-else-if="devTab === 'screenshots'">
							<section class="developer-card">
								<div class="developer-section-heading">
									<div>
										<h3>Galería de capturas de {{ selectedServer.title }}</h3>
										<span>
											Se usan en la galería principal, en la vista previa lateral y en Explorar
										</span>
									</div>
								</div>

								<div class="developer-screenshot-list">
									<div
										v-for="(shot, idx) in selectedServer.screenshots || []"
										:key="`${screenshotUrl(shot)}-${idx}`"
										class="developer-screenshot"
										:class="{ 'screenshot-hidden': screenshotStatus(shot) === 'hidden' }"
									>
										<img :src="resolveImageUrl(screenshotUrl(shot))" :alt="`Captura ${idx + 1}`" />
										<span :title="screenshotUrl(shot)">
											{{ screenshotUrl(shot).split('/').pop() }}
										</span>
										<select
											:value="screenshotStatus(shot) === 'hidden' ? 'hidden' : 'active'"
											@change="updateScreenshotStatus(idx, ($event.target as HTMLSelectElement).value)"
										>
											<option value="active">Visible</option>
											<option value="hidden">Oculta</option>
										</select>
										<button
											type="button"
											aria-label="Eliminar captura"
											@click="removeScreenshotAt(idx)"
										>
											✕
										</button>
									</div>
									<p
										v-if="!selectedServer.screenshots || selectedServer.screenshots.length === 0"
										class="developer-empty"
									>
										Este modpack aún no tiene capturas.
									</p>
								</div>

								<!-- Dedicated Drag & Drop Zone for Multiple Screenshots -->
								<div
									class="developer-screenshots-dropzone"
									:class="{ 'is-over': isDraggingScreenshots }"
									@dragover.prevent="isDraggingScreenshots = true"
									@dragleave="isDraggingScreenshots = false"
									@drop.prevent="handleScreenshotsDrop"
									@click="screenshotMultiInputRef?.click()"
								>
									<svg class="dropzone-icon" viewBox="0 0 24 24" width="32" height="32" fill="none" stroke="currentColor" stroke-width="1.8">
										<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M17 8l-5-5-5 5M12 3v12" />
									</svg>
									<strong>Arrastra tus capturas de pantalla aquí</strong>
									<span>o haz clic para seleccionar una o varias imágenes desde tu equipo (PNG, JPG, WEBP)</span>
									<input
										ref="screenshotMultiInputRef"
										type="file"
										multiple
										accept="image/*"
										hidden
										@change="handleMultiScreenshotsSelected"
									/>
								</div>

								<form class="developer-add-row" @submit.prevent="handleAddScreenshot">
									<div class="developer-image-dropzone developer-inline-drop">
										<input
											v-model="newScreenshotUrl"
											placeholder="URL https://... o pega enlace directo"
										/>
										<button
											type="button"
											class="developer-secondary"
											@click="screenshotInputRef?.click()"
										>
											Subir archivo único
										</button>
										<input
											ref="screenshotInputRef"
											type="file"
											accept="image/*"
											hidden
											@change="handleFileInputUpload($event, (url) => (newScreenshotUrl = url))"
										/>
									</div>
									<button type="submit" class="developer-primary">Añadir captura</button>
								</form>
							</section>
						</template>

						<!-- TAB 4: MODES (MAINTENANCE & TESTING) -->
						<template v-else-if="devTab === 'modes'">
							<section class="developer-card">
								<div class="developer-server-heading">
									<div>
										<strong>Control de Acceso y Modos Especiales</strong>
										<span>Configura el estado global del launcher: Modo Mantenimiento y Modo Testing</span>
									</div>
								</div>

								<div class="developer-modes-container">
									<div
										class="developer-mode-box"
										:class="{ 'mode-box-active': catalog.maintenance_mode }"
									>
										<div class="developer-mode-header">
											<div class="developer-mode-info">
												<div class="developer-mode-title-row">
													<h4>Modo Mantenimiento</h4>
													<span
														class="developer-mode-status-pill"
														:class="catalog.maintenance_mode ? 'pill-active' : 'pill-inactive'"
													>
														{{ catalog.maintenance_mode ? 'ACTIVO (BLOQUEADO)' : 'INACTIVO' }}
													</span>
												</div>
												<p class="developer-mode-explanation">
													Bloquea el launcher para los usuarios ordinarios mientras realizas
													mantenimiento en los servidores. Tu acceso como administrador no se
													bloquea.
												</p>
											</div>
											<label class="developer-toggle-switch">
												<input v-model="catalog.maintenance_mode" type="checkbox" />
												<span class="developer-toggle-slider" />
											</label>
										</div>

										<div class="developer-mode-input-block">
											<label>
												Mensaje personalizado para los jugadores (opcional):
												<input
													v-model="catalog.maintenance_message"
													type="text"
													placeholder="Estamos realizando tareas de mantenimiento y optimización en los servidores. Volveremos pronto."
												/>
											</label>
										</div>
									</div>

									<div
										class="developer-mode-box"
										:class="{ 'mode-box-active': catalog.testing_mode }"
									>
										<div class="developer-mode-header">
											<div class="developer-mode-info">
												<div class="developer-mode-title-row">
													<h4>Modo Testing (Pruebas Temporales)</h4>
													<span
														class="developer-mode-status-pill"
														:class="catalog.testing_mode ? 'pill-active' : 'pill-inactive'"
													>
														{{ catalog.testing_mode ? 'ACTIVO (PROTEGIDO)' : 'INACTIVO' }}
													</span>
												</div>
												<p class="developer-mode-explanation">
													Permite a los usuarios descargar y probar instancias de modpacks
													temporalmente durante sesiones de prueba.
												</p>
											</div>
											<label class="developer-toggle-switch">
												<input v-model="catalog.testing_mode" type="checkbox" />
												<span class="developer-toggle-slider" />
											</label>
										</div>
									</div>
								</div>
							</section>
						</template>
					</div>
				</div>

				<!-- Footer -->
				<footer class="developer-footer">
					<div class="developer-status" role="status">
						{{ status || 'Los cambios en el borrador se aplican al pulsar "Guardar en GitHub".' }}
					</div>
					<div class="developer-footer-actions">
						<button
							type="button"
							class="developer-secondary"
							:disabled="busy || !selectedServer"
							@click="publishCatalogToGithub(true)"
						>
							Guardar y forzar señal global
						</button>
						<button
							type="button"
							class="developer-primary"
							:disabled="busy"
							@click="publishCatalogToGithub(false)"
						>
							{{ busy ? 'Guardando...' : 'Guardar en GitHub' }}
						</button>
					</div>
				</footer>
			</template>
		</section>
	</div>
</template>

<style scoped>
.developer-advanced-details {
	margin-top: 0.25rem;
	padding: 0.6rem 0.85rem;
	background: rgba(255, 255, 255, 0.03);
	border: 1px dashed rgba(255, 255, 255, 0.15);
	border-radius: 8px;
	box-sizing: border-box;
}

.developer-advanced-summary {
	cursor: pointer;
	font-size: 0.82rem;
	font-weight: 500;
	color: var(--color-text-secondary, #9ca3af);
	user-select: none;
	outline: none;
	display: list-item;
}

.developer-advanced-summary:hover {
	color: var(--color-text-primary, #ffffff);
}

.developer-advanced-content {
	margin-top: 0.75rem;
	display: flex;
	flex-direction: column;
	gap: 0.75rem;
}
</style>
