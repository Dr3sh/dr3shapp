import { computed, ref, watch } from 'vue'
import { getVersion } from '@tauri-apps/api/app'
import { invoke } from '@tauri-apps/api/core'
import { fetch as tauriFetch } from '@tauri-apps/plugin-http'
import pkg from '../../package.json'
import {
	add_offline_user,
	get_default_user,
	login as tauriMicrosoftLogin,
	remove_user,
	set_default_user,
	users,
} from '@/helpers/auth'

export const LAUNCHER_VERSION = pkg.version || '1.1.2'
export const launcherVersion = ref<string>(LAUNCHER_VERSION)

getVersion()
	.then((v) => {
		if (v && v !== '0.0.0') {
			launcherVersion.value = v.replace(/^v/i, '')
		}
	})
	.catch(() => {})

export type ScreenshotEntry = string | { url: string; status?: string; [key: string]: unknown }

export interface Dr3shServer {
	id: string
	title: string
	aliases?: string[]
	status?: string
	image?: string
	logo?: string
	logo_square?: string
	title_logo?: string
	explore_image?: string
	theme_color?: string
	description?: string
	minecraft_version?: string
	mod_loader?: string
	mod_loader_version?: string
	screenshots?: ScreenshotEntry[]
	server_address?: string
	versions?: Dr3shVersion[]
	disabled?: boolean
	disabled_message?: string
	release_date?: string
	preload_hours_before?: number
}

export interface Dr3shVersionPatch {
	from: string
	url: string
	size?: number
	sha256?: string
}

export interface Dr3shVersion {
	id: string
	server: string
	title: string
	version?: string
	description?: string
	link?: string
	manifest?: string
	patch?: Dr3shVersionPatch | null
	sha256?: string
	minecraft_version?: string
	mod_loader?: string
	mod_loader_version?: string
	size_mb?: number
	published_at?: string
	status?: string
}

export interface Dr3shAccount {
	id: string
	name: string
	uuid: string
	isMicrosoft: boolean
}

export const DESGRACIAMON2_SCREENSHOTS = [
	'https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/4fc7f15191920824c38f09b2761ec2a641b5d941/assets/screenshots/desgraciamon2.%20(1).jpg',
	'https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/4fc7f15191920824c38f09b2761ec2a641b5d941/assets/screenshots/desgraciamon2.%20(2).jpg',
	'https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/4fc7f15191920824c38f09b2761ec2a641b5d941/assets/screenshots/desgraciamon2.%20(3).jpg%2E',
	'https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/4fc7f15191920824c38f09b2761ec2a641b5d941/assets/screenshots/desgraciamon2.%20(4).jpg',
	'https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/4fc7f15191920824c38f09b2761ec2a641b5d941/assets/screenshots/desgraciamon2.%20(5).jpg',
]

export const servers = ref<Dr3shServer[]>([])
export const versions = ref<Dr3shVersion[]>([])
export const allCatalogServers = ref<Dr3shServer[]>([])
export const allCatalogVersions = ref<Dr3shVersion[]>([])
const loading = ref(true)
export const isOffline = ref<boolean>(typeof navigator !== 'undefined' ? !navigator.onLine : false)
export const catalogError = ref<string | null>(null)
export const retryingCatalog = ref<boolean>(false)

const DEFAULT_AVATAR_SVG = `data:image/svg+xml;utf8,${encodeURIComponent(
	'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#131824"/><rect x="8" y="8" width="16" height="16" rx="3" fill="#283246"/><rect x="11" y="13" width="3" height="3" rx="0.5" fill="#94a3b8"/><rect x="18" y="13" width="3" height="3" rx="0.5" fill="#94a3b8"/><rect x="13" y="19" width="6" height="2" rx="0.5" fill="#64748b"/></svg>',
)}`

export function handleAvatarError(event: Event) {
	const img = event.target as HTMLImageElement | null
	if (!img || img.dataset.avatarFallback) return
	img.dataset.avatarFallback = '1'
	img.src = DEFAULT_AVATAR_SVG
}

if (typeof window !== 'undefined') {
	window.addEventListener('offline', () => {
		isOffline.value = true
	})
	window.addEventListener('online', () => {
		isOffline.value = false
		void fetchVersionsData()
	})
}

export const maintenanceMode = ref(false)
export const maintenanceMessage = ref('')
export const testingMode = ref(false)
export const testingHours = ref(1)
export const launcherRefreshMode = ref<'automatic' | 'manual_global' | 'manual_user'>('automatic')
export const launcherRefreshId = ref('initial')
export const developerToken = ref<string>(
	(
		localStorage.getItem('dr3sh.developer_token') ||
		localStorage.getItem('dr3sh.github_token') ||
		''
	).trim(),
)

export function setDeveloperToken(token: string) {
	const clean = token.trim()
	developerToken.value = clean
	try {
		if (clean) {
			localStorage.setItem('dr3sh.developer_token', clean)
			localStorage.setItem('dr3sh.github_token', clean)
		} else {
			localStorage.removeItem('dr3sh.developer_token')
			localStorage.removeItem('dr3sh.github_token')
		}
	} catch {}
}

void invoke<{ token?: string } | null>('dr3sh_load_dev_session')
	.then((session) => {
		const saved = session?.token?.trim()
		if (saved && !developerToken.value) {
			setDeveloperToken(saved)
		}
	})
	.catch(() => {})

export const isDeveloper = computed(() => {
	try {
		const token = developerToken.value.trim()
		if (!token) return false
		// Require genuine GitHub PAT or token format to prevent trivial localStorage bypasses
		const isClassicPat = /^ghp_[a-zA-Z0-9]{36}$/.test(token)
		const isFineGrainedPat = /^github_pat_[a-zA-Z0-9_]{82,}$/.test(token)
		const isOAuthToken = /^(gho_|ghu_|ghs_)[a-zA-Z0-9_]{36,}$/.test(token)
		const isLongValidHex = /^[a-fA-F0-9]{40}$/.test(token)
		return isClassicPat || isFineGrainedPat || isOAuthToken || isLongValidHex
	} catch {
		return false
	}
})

// Persisted owned servers - default to empty (0 servers) so user explores to add
function loadInitialOwned(): Set<string> {
	try {
		// Migrate older users who were forced into the 4 hardcoded servers
		const migrationDone = localStorage.getItem('dr3sh.owned_migrated_v2')
		if (!migrationDone) {
			localStorage.setItem('dr3sh.owned_migrated_v2', 'true')
			localStorage.removeItem('dr3sh_owned_servers')
			localStorage.removeItem('dr3sh.owned')
			return new Set<string>([])
		}
		const raw = localStorage.getItem('dr3sh_owned_servers') || localStorage.getItem('dr3sh.owned')
		if (raw) {
			const arr = JSON.parse(raw)
			if (Array.isArray(arr)) return new Set(arr)
		}
	} catch {}
	return new Set<string>([])
}

export const owned = ref<Set<string>>(loadInitialOwned())

function persistOwned() {
	try {
		const serialized = JSON.stringify(Array.from(owned.value))
		localStorage.setItem('dr3sh_owned_servers', serialized)
		localStorage.setItem('dr3sh.owned', serialized)
	} catch {}
}

watch(
	owned,
	() => {
		persistOwned()
	},
	{ deep: true },
)

export const isSidebarCollapsed = ref<boolean>(
	typeof localStorage !== 'undefined' && localStorage.getItem('dr3sh.sidebar_collapsed') === 'true',
)
export const railWidth = ref<number>(
	isSidebarCollapsed.value
		? 72
		: typeof localStorage !== 'undefined' && localStorage.getItem('dr3sh.sidebar_width')
		? Number(localStorage.getItem('dr3sh.sidebar_width')) || 268
		: 268,
)

export function setRailWidth(widthPx: number, isDragging = false) {
	if (widthPx < 140) {
		isSidebarCollapsed.value = true
		railWidth.value = 72
		if (typeof localStorage !== 'undefined') {
			localStorage.setItem('dr3sh.sidebar_collapsed', 'true')
		}
	} else {
		isSidebarCollapsed.value = false
		railWidth.value = Math.min(420, Math.max(200, widthPx))
		if (typeof localStorage !== 'undefined') {
			localStorage.setItem('dr3sh.sidebar_collapsed', 'false')
			localStorage.setItem('dr3sh.sidebar_width', String(railWidth.value))
		}
	}
	if (typeof document !== 'undefined') {
		document.documentElement.style.setProperty('--rail-width', `${railWidth.value}px`)
		if (isDragging) {
			document.body.classList.add('is-resizing-rail')
		} else {
			document.body.classList.remove('is-resizing-rail')
		}
	}
}

export function toggleSidebarCollapse() {
	if (isSidebarCollapsed.value) {
		const savedWidth =
			typeof localStorage !== 'undefined' && localStorage.getItem('dr3sh.sidebar_width')
				? Number(localStorage.getItem('dr3sh.sidebar_width')) || 268
				: 268
		setRailWidth(savedWidth, false)
	} else {
		setRailWidth(72, false)
	}
}

if (typeof document !== 'undefined') {
	document.documentElement.style.setProperty(
		'--rail-width',
		`${railWidth.value}px`,
	)
}

export const selectedId = ref<string>(localStorage.getItem('dr3sh_selected_server') ?? 'desgraciamon3')

watch(selectedId, (val) => {
	if (val) localStorage.setItem('dr3sh_selected_server', val)
	else localStorage.removeItem('dr3sh_selected_server')
})

export const currentView = ref<'home' | 'market' | 'settings'>((localStorage.getItem('dr3sh_current_view') as 'home' | 'market' | 'settings') || 'home')
export const activeSettingsTab = ref<'memory' | 'account' | 'game' | 'storage'>((localStorage.getItem('dr3sh_settings_tab') as 'memory' | 'account' | 'game' | 'storage') || 'memory')

watch(currentView, (val) => {
	if (val) localStorage.setItem('dr3sh_current_view', val)
})

watch(activeSettingsTab, (val) => {
	if (val) localStorage.setItem('dr3sh_settings_tab', val)
})

watch([selectedId, currentView], () => {
	const currentServer = servers.value.find((s) => s.id === selectedId.value)
	updateThemePalette(currentView.value === 'home' ? currentServer : null)
})

export const activeAccount = ref<Dr3shAccount | null>(null)
export const allAccounts = ref<Dr3shAccount[]>([])

export async function refreshAccount() {
	try {
		let defaultId = await get_default_user()
		let rawUsers = await users()

		// Ensure at least one default offline account exists so launching Minecraft never fails with NotLoggedIn
		if (!Array.isArray(rawUsers) || rawUsers.length === 0) {
			const fallbackName = localStorage.getItem('dr3sh.last_username') || 'Player'
			try {
				const created = await add_offline_user(fallbackName)
				const createdId = created?.profile?.id || created?.offline_profile?.id
				if (createdId) {
					await set_default_user(createdId)
					defaultId = createdId
				}
				rawUsers = await users()
			} catch (seedErr) {
				console.warn('Could not seed initial offline account:', seedErr)
			}
		}

		if (Array.isArray(rawUsers) && rawUsers.length > 0) {
			const mapped: Dr3shAccount[] = rawUsers.map((u: any) => {
				const prof = u.profile || u.offline_profile || {}
				const isMs = Boolean(u.refresh_token && u.refresh_token !== 'offline')
				return {
					id: prof.id || '',
					name: prof.name || 'Player',
					uuid: prof.id || '',
					isMicrosoft: isMs,
				}
			})
			allAccounts.value = mapped

			const active = mapped.find((u) => u.id === defaultId) || mapped[0]
			if (active) {
				activeAccount.value = active
				if (!defaultId && active.id) {
					try {
						await set_default_user(active.id)
					} catch {}
				}
				return
			}
		}
	} catch (e) {
		console.warn('Error refreshing active account:', e)
	}
	activeAccount.value = null
}

export async function loginOfflineAccount(username: string): Promise<Dr3shAccount | null> {
	const clean = username.trim()
	if (!clean) return null
	localStorage.setItem('dr3sh.last_username', clean)
	const creds = await add_offline_user(clean)
	const id = creds?.profile?.id || creds?.offline_profile?.id
	if (id) {
		await set_default_user(id)
	}
	await refreshAccount()
	return activeAccount.value
}

export async function loginMicrosoftAccount(): Promise<Dr3shAccount | null> {
	const creds = await tauriMicrosoftLogin()
	if (creds) {
		const id = creds?.profile?.id || creds?.offline_profile?.id
		if (id) {
			await set_default_user(id)
		}
		await refreshAccount()
	}
	return activeAccount.value
}

export async function switchActiveAccount(accountId: string) {
	if (!accountId) return
	await set_default_user(accountId)
	await refreshAccount()
}

export async function removeSavedAccount(accountId: string) {
	if (!accountId) return
	await remove_user(accountId)
	await refreshAccount()
}

export function mixHex(a: string, b: string, ratio: number) {
	const left = a.match(/[\da-f]{2}/gi)?.map((part) => parseInt(part, 16)) ?? [13, 16, 23]
	const right = b.match(/[\da-f]{2}/gi)?.map((part) => parseInt(part, 16)) ?? [47, 101, 246]
	return `#${left
		.map((value, index) => Math.round(value + (right[index] - value) * ratio).toString(16).padStart(2, '0'))
		.join('')}`
}

function hexToHsl(hex: string): [number, number, number] {
	const parts = hex.match(/[\da-f]{2}/gi)?.map((p) => parseInt(p, 16) / 255) ?? [0.18, 0.4, 0.96]
	const [r, g, b] = parts
	const max = Math.max(r, g, b)
	const min = Math.min(r, g, b)
	let h = 0
	let s = 0
	const l = (max + min) / 2

	if (max !== min) {
		const d = max - min
		s = l > 0.5 ? d / (2 - max - min) : d / (max + min)
		switch (max) {
			case r:
				h = (g - b) / d + (g < b ? 6 : 0)
				break
			case g:
				h = (b - r) / d + 2
				break
			case b:
				h = (r - g) / d + 4
				break
		}
		h /= 6
	}
	return [h, s, l]
}

function hslToHex(h: number, s: number, l: number): string {
	const hue2rgb = (p: number, q: number, t: number) => {
		let tt = t
		if (tt < 0) tt += 1
		if (tt > 1) tt -= 1
		if (tt < 1 / 6) return p + (q - p) * 6 * tt
		if (tt < 1 / 2) return q
		if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6
		return p
	}
	let r: number, g: number, b: number
	if (s === 0) {
		r = g = b = l
	} else {
		const q = l < 0.5 ? l * (1 + s) : l + s - l * s
		const p = 2 * l - q
		r = hue2rgb(p, q, h + 1 / 3)
		g = hue2rgb(p, q, h)
		b = hue2rgb(p, q, h - 1 / 3)
	}
	return `#${[r, g, b]
		.map((x) => Math.round(x * 255).toString(16).padStart(2, '0'))
		.join('')}`
}

export function getRawServerColor(server?: Dr3shServer | null): string {
	if (!server) return '#1bd96a'
	const raw = (server as any).theme_color || (server as any).color
	if (raw && /^#[\da-f]{6}$/i.test(String(raw).trim())) {
		return String(raw).trim()
	}
	return '#1bd96a'
}

export function getSubduedColor(hex: string): string {
	const [h, s, l] = hexToHsl(hex)
	// Si el color base es gris/neutro, conservarlo gris plateado (evitar forzar saturación que lo vuelve marrón)
	if (s < 0.1) {
		const neutralL = Math.max(0.46, Math.min(0.64, l))
		return hslToHex(0, Math.min(s, 0.04), neutralL)
	}
	// Acento vivo y con identidad sobre negro AMOLED, evitando neones chillones
	const newS = Math.max(0.32, Math.min(0.80, s * 0.88))
	const newL = Math.max(0.38, Math.min(0.54, l * 0.94))
	return hslToHex(h, newS, newL)
}

export function getAccentColor(server?: Dr3shServer | null): string {
	if (!server) return '#1bd96a'
	return getSubduedColor(getRawServerColor(server))
}

export function getContrastTextColor(hex: string): '#0b0e14' | '#ffffff' {
	const clean = String(hex || '').replace('#', '').trim()
	if (clean.length !== 6) return '#ffffff'
	const toLinear = (c: number) => {
		const s = c / 255
		return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4)
	}
	const r = toLinear(parseInt(clean.slice(0, 2), 16))
	const g = toLinear(parseInt(clean.slice(2, 4), 16))
	const b = toLinear(parseInt(clean.slice(4, 6), 16))
	if (Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b)) return '#ffffff'
	const luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b
	return luminance > 0.24 ? '#0b0e14' : '#ffffff'
}

const INITIAL_COLORS = {
	'--ink': '#050608',
	'--panel': '#0a0c10',
	'--panel-2': '#11141b',
	'--line': '#1b202c',
}

try {
	if (typeof document !== 'undefined') {
		const initialView = localStorage.getItem('dr3sh_current_view') || 'home'
		const initialServer = localStorage.getItem('dr3sh_selected_server')
		const initialAccent = localStorage.getItem('dr3sh_active_accent')
		const root = document.documentElement
		if (initialView === 'home' && initialServer && initialAccent) {
			const subduedAccent = getSubduedColor(initialAccent)
			const onAccent = getContrastTextColor(subduedAccent)
			root.style.setProperty('--ink', mixHex(INITIAL_COLORS['--ink'], subduedAccent, 0.06))
			root.style.setProperty('--panel', mixHex(INITIAL_COLORS['--panel'], subduedAccent, 0.09))
			root.style.setProperty('--panel-2', mixHex(INITIAL_COLORS['--panel-2'], subduedAccent, 0.12))
			root.style.setProperty('--line', mixHex(INITIAL_COLORS['--line'], subduedAccent, 0.22))
			root.style.setProperty('--brand', subduedAccent)
			root.style.setProperty('--on-brand', onAccent)
			root.style.setProperty('--brand-hover', mixHex(subduedAccent, '#ffffff', 0.14))
			root.style.setProperty('--brand-soft', mixHex('#050608', subduedAccent, 0.22))
			root.style.setProperty('--current-accent', subduedAccent)
			root.style.setProperty('--current-on-accent', onAccent)
			root.style.setProperty(
				'--current-on-accent-shadow',
				onAccent === '#0b0e14' ? 'none' : '0 1px 3px rgba(0, 0, 0, 0.38)',
			)
			root.style.setProperty('--current-glow', `color-mix(in srgb, ${subduedAccent} 48%, transparent)`)
			root.style.setProperty('--current-border', `color-mix(in srgb, ${subduedAccent} 46%, transparent)`)
			root.style.setProperty('--current-soft', `color-mix(in srgb, ${subduedAccent} 24%, transparent)`)
		} else {
			const baseOnAccent = getContrastTextColor('#1bd96a')
			root.style.setProperty('--brand', '#1bd96a')
			root.style.setProperty('--on-brand', baseOnAccent)
			root.style.setProperty('--brand-hover', '#16bd5a')
			root.style.setProperty('--brand-soft', 'rgba(27, 217, 106, 0.18)')
			root.style.setProperty('--current-accent', '#1bd96a')
			root.style.setProperty('--current-on-accent', baseOnAccent)
			root.style.setProperty(
				'--current-on-accent-shadow',
				baseOnAccent === '#0b0e14' ? 'none' : '0 1px 3px rgba(0, 0, 0, 0.38)',
			)
			root.style.setProperty('--current-glow', 'rgba(27, 217, 106, 0.45)')
			root.style.setProperty('--current-border', 'rgba(27, 217, 106, 0.35)')
			root.style.setProperty('--current-soft', 'rgba(27, 217, 106, 0.15)')
		}
	}
} catch {}

export function updateThemePalette(server?: Dr3shServer | null) {
	if (typeof document === 'undefined') return
	const root = document.documentElement
	if (!server || currentView.value !== 'home') {
		localStorage.removeItem('dr3sh_active_accent')
		const baseOnAccent = getContrastTextColor('#1bd96a')
		// Reset to Verde Base (#1bd96a)
		root.style.setProperty('--brand', '#1bd96a')
		root.style.setProperty('--on-brand', baseOnAccent)
		root.style.setProperty('--brand-hover', '#16bd5a')
		root.style.setProperty('--brand-soft', 'rgba(27, 217, 106, 0.18)')
		root.style.setProperty('--current-accent', '#1bd96a')
		root.style.setProperty('--current-on-accent', baseOnAccent)
		root.style.setProperty(
			'--current-on-accent-shadow',
			baseOnAccent === '#0b0e14' ? 'none' : '0 1px 3px rgba(0, 0, 0, 0.38)',
		)
		root.style.setProperty('--current-glow', 'rgba(27, 217, 106, 0.45)')
		root.style.setProperty('--current-border', 'rgba(27, 217, 106, 0.35)')
		root.style.setProperty('--current-soft', 'rgba(27, 217, 106, 0.15)')
		root.style.setProperty('--ink', INITIAL_COLORS['--ink'])
		root.style.setProperty('--panel', INITIAL_COLORS['--panel'])
		root.style.setProperty('--panel-2', INITIAL_COLORS['--panel-2'])
		root.style.setProperty('--line', INITIAL_COLORS['--line'])
		return
	}
	const brandAccent = getAccentColor(server)
	const onAccent = getContrastTextColor(brandAccent)
	localStorage.setItem('dr3sh_active_accent', brandAccent)

	// Dos tonos sobre negro AMOLED: superficies casi negras con tinte sutil + acento vivo controlado
	root.style.setProperty('--ink', mixHex(INITIAL_COLORS['--ink'], brandAccent, 0.06))
	root.style.setProperty('--panel', mixHex(INITIAL_COLORS['--panel'], brandAccent, 0.09))
	root.style.setProperty('--panel-2', mixHex(INITIAL_COLORS['--panel-2'], brandAccent, 0.12))
	root.style.setProperty('--line', mixHex(INITIAL_COLORS['--line'], brandAccent, 0.22))
	root.style.setProperty('--brand', brandAccent)
	root.style.setProperty('--on-brand', onAccent)
	root.style.setProperty('--brand-hover', mixHex(brandAccent, '#ffffff', 0.14))
	root.style.setProperty('--brand-soft', mixHex('#050608', brandAccent, 0.22))
	root.style.setProperty('--current-accent', brandAccent)
	root.style.setProperty('--current-on-accent', onAccent)
	root.style.setProperty(
		'--current-on-accent-shadow',
		onAccent === '#0b0e14' ? 'none' : '0 1px 3px rgba(0, 0, 0, 0.38)',
	)
	root.style.setProperty('--current-glow', `color-mix(in srgb, ${brandAccent} 48%, transparent)`)
	root.style.setProperty('--current-border', `color-mix(in srgb, ${brandAccent} 46%, transparent)`)
	root.style.setProperty('--current-soft', `color-mix(in srgb, ${brandAccent} 24%, transparent)`)
}

export function deselectModpack() {
	selectedId.value = ''
	localStorage.removeItem('dr3sh_selected_server')
	updateThemePalette(null)
}

export function resolveImageUrl(img?: string | null): string {
	if (!img) return ''
	let cleaned = String(img).trim()
	// Encode trailing dot on filenames like desgraciamon2.(3).jpg. as %2E so Windows/Chromium never strips it (preventing 404)
	cleaned = cleaned.replace(/\.(jpg|jpeg|png|webp|gif)\.$/i, '.$1%2E')
	cleaned = cleaned.replace(/ /g, '%20')
	if (cleaned.startsWith('http://') || cleaned.startsWith('https://') || cleaned.startsWith('data:') || cleaned.startsWith('/')) {
		return cleaned
	}
	return `https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/${cleaned.replace(/^\/+/, '')}`
}

const MEDIA_CACHE_NAME = 'dr3sh-media-cache-v1'
const cachedMediaUrls = ref<Record<string, string>>({})
const inflightMediaFetches = new Set<string>()
const mediaQueue: string[] = []
let activeMediaWorkers = 0

async function processMediaQueue() {
	if (typeof window === 'undefined' || !('caches' in window)) return
	while (activeMediaWorkers < 4 && mediaQueue.length > 0) {
		const url = mediaQueue.shift()
		if (!url) continue
		activeMediaWorkers++
		void (async () => {
			try {
				const cache = await window.caches.open(MEDIA_CACHE_NAME)
				const cachedResp = await cache.match(url)
				if (cachedResp) {
					const blob = await cachedResp.blob()
					if (blob.size > 0) {
						cachedMediaUrls.value = {
							...cachedMediaUrls.value,
							[url]: URL.createObjectURL(blob),
						}
						return
					}
				}
				const res = await tauriFetch(url).catch(() => null)
				if (!res || !res.ok) return
				const buf = await res.arrayBuffer()
				if (!buf || buf.byteLength <= 0 || buf.byteLength > 15 * 1024 * 1024) return
				const contentType = res.headers?.get?.('content-type') || 'image/webp'
				const blob = new Blob([buf], { type: contentType })
				await cache.put(
					url,
					new Response(blob, {
						status: 200,
						headers: { 'Content-Type': contentType },
					}),
				)
				cachedMediaUrls.value = {
					...cachedMediaUrls.value,
					[url]: URL.createObjectURL(blob),
				}
			} catch {
				// Best-effort cache; network URL fallback remains active
			} finally {
				activeMediaWorkers--
				void processMediaQueue()
			}
		})()
	}
}

function queueCacheMediaUrl(remoteUrl: string) {
	if (!remoteUrl || !remoteUrl.startsWith('https://')) return
	if (typeof window === 'undefined' || !('caches' in window)) return
	if (cachedMediaUrls.value[remoteUrl] || inflightMediaFetches.has(remoteUrl)) return
	inflightMediaFetches.add(remoteUrl)
	mediaQueue.push(remoteUrl)
	void processMediaQueue()
}

export function getCachedMediaUrl(remoteUrl: string): string {
	if (!remoteUrl || !remoteUrl.startsWith('https://')) return remoteUrl
	const hit = cachedMediaUrls.value[remoteUrl]
	if (hit) return hit
	queueCacheMediaUrl(remoteUrl)
	return remoteUrl
}

export function getStageImageUrl(url?: string | null): string {
	const resolved = resolveImageUrl(url)
	if (!resolved) return '/logo.png'
	// Compress 3MB+ uncompressed PNG uploads via global WebP edge cache for instant display
	if (resolved.includes('raw.githubusercontent.com') && resolved.includes('/assets/uploads/') && /\.png$/i.test(resolved)) {
		return getCachedMediaUrl(
			`https://wsrv.nl/?url=${encodeURIComponent(resolved)}&w=1280&q=85&output=webp`,
		)
	}
	return getCachedMediaUrl(resolved)
}

export function getThumbnailUrl(url?: string | null): string {
	const resolved = resolveImageUrl(url)
	if (!resolved) return '/logo.png'
	if (resolved.includes('raw.githubusercontent.com') && (resolved.includes('/assets/uploads/') || resolved.includes('/assets/screenshots/')) && /\.(png|jpg|jpeg)$/i.test(resolved)) {
		return getCachedMediaUrl(
			`https://wsrv.nl/?url=${encodeURIComponent(resolved)}&w=260&q=80&output=webp`,
		)
	}
	return getCachedMediaUrl(resolved)
}

export function handleImageError(event: Event, rawFallbackUrl?: string) {
	const img = event.target as HTMLImageElement | null
	if (!img) return
	const currentSrc = img.src || ''
	if (rawFallbackUrl) {
		const resolvedRaw = resolveImageUrl(rawFallbackUrl)
		if (resolvedRaw && currentSrc !== resolvedRaw && !img.dataset.retriedRaw) {
			img.dataset.retriedRaw = '1'
			img.src = resolvedRaw
			return
		}
	}
	// If trailing dot toggle could help
	if (currentSrc.endsWith('.jpg') && !img.dataset.retriedDot) {
		img.dataset.retriedDot = '1'
		img.src = `${currentSrc}%2E`
		return
	}
	if (!currentSrc.endsWith('/logo.png')) {
		img.src = '/logo.png'
	}
}

export function getServerPreviewImages(server?: Dr3shServer | null): string[] {
	if (!server) return []
	const fromServer = (server.screenshots ?? [])
		.filter((item) => typeof item === 'string' || String(item.status || 'active') !== 'hidden')
		.map((item) => (typeof item === 'string' ? item : item.url))
		.filter((url): url is string => Boolean(url && String(url).trim()))
		.map((url) => resolveImageUrl(url))

	if (fromServer.length > 0) return fromServer
	if (server.id === 'desgraciamon2') return DESGRACIAMON2_SCREENSHOTS

	const fallback = [server.image, server.logo, server.logo_square]
		.filter((url): url is string => Boolean(url && String(url).trim()))
		.map((url) => resolveImageUrl(url))

	return fallback.length > 0 ? fallback : (server.id === 'desgraciamon2' ? DESGRACIAMON2_SCREENSHOTS : [])
}

export function hasCustomScreenshots(server?: Dr3shServer | null): boolean {
	if (!server) return false
	const fromServer = (server.screenshots ?? []).filter(
		(item) => typeof item === 'string' || String(item.status || 'active') !== 'hidden',
	)
	return fromServer.length > 0 || server.id === 'desgraciamon2'
}

// Official Modrinth Loader Vector Icons (from @modrinth/assets/icons/tags/loaders/*.svg)
const MODRINTH_LOADER_SVGS: Record<string, string> = {
	fabric: `data:image/svg+xml;utf8,${encodeURIComponent(
		'<svg xmlns="http://www.w3.org/2000/svg" fill-rule="evenodd" stroke-linecap="round" stroke-linejoin="round" clip-rule="evenodd" viewBox="0 0 24 24"><path fill="none" stroke="#f1f5f9" stroke-width="23" d="m820 761-85.6-87.6c-4.6-4.7-10.4-9.6-25.9 1-19.9 13.6-8.4 21.9-5.2 25.4 8.2 9 84.1 89 97.2 104 2.5 2.8-20.3-22.5-6.5-39.7 5.4-7 18-12 26-3 6.5 7.3 10.7 18-3.4 29.7-24.7 20.4-102 82.4-127 103-12.5 10.3-28.5 2.3-35.8-6-7.5-8.9-30.6-34.6-51.3-58.2-5.5-6.3-4.1-19.6 2.3-25 35-30.3 91.9-73.8 111.9-90.8" transform="matrix(.08671 0 0 .0867 -49.8 -56)"/></svg>',
	)}`,
	forge: `data:image/svg+xml;utf8,${encodeURIComponent(
		'<svg xmlns="http://www.w3.org/2000/svg" fill-rule="evenodd" stroke-linecap="round" stroke-linejoin="round" stroke-miterlimit="1.5" clip-rule="evenodd" viewBox="0 0 24 24"><path fill="none" stroke="#f1f5f9" stroke-width="2" d="M2 7.5h8v-2h12v2s-7 3.4-7 6 3.1 3.1 3.1 3.1l.9 3.9H5l1-4.1s3.8.1 4-2.9c.2-2.7-6.5-.7-8-6Z"/></svg>',
	)}`,
	neoforge: `data:image/svg+xml;utf8,${encodeURIComponent(
		'<svg xmlns="http://www.w3.org/2000/svg" fill-rule="evenodd" stroke-linecap="round" stroke-linejoin="round" stroke-miterlimit="1.5" clip-rule="evenodd" viewBox="0 0 100 100"><g fill="none" stroke="#f1f5f9" stroke-width="8.33"><path d="M61.098 32.028V6.529L84.001 26.74v52.944L50.38 93.608 16.759 79.684V26.74L39.662 6.529v25.499M39.805 25.427h21.15M34.23 54.498v7.815M65.77 54.498v7.815M47.035 77.633h6.692M21.719 51.608 6.492 44.885M21.719 63.582 6.492 70.305M79.043 51.608l15.227-6.723M79.043 63.582l15.227 6.723"/></g></svg>',
	)}`,
	quilt: `data:image/svg+xml;utf8,${encodeURIComponent(
		'<svg xmlns="http://www.w3.org/2000/svg" fill-rule="evenodd" stroke-linecap="round" stroke-linejoin="round" stroke-miterlimit="2" clip-rule="evenodd" viewBox="0 0 24 24"><path fill="none" stroke="#f1f5f9" stroke-width="65.6" d="M442.5 233.9c0-6.4-5.2-11.6-11.6-11.6h-197c-6.4 0-11.6 5.2-11.6 11.6v197c0 6.4 5.2 11.6 11.6 11.6h197c6.4 0 11.6-5.2 11.6-11.7v-197 .1Z" transform="matrix(.03053 0 0 .03046 -3.2 -3.2)"/><path fill="none" stroke="#f1f5f9" stroke-width="65.6" d="M442.5 233.9c0-6.4-5.2-11.6-11.6-11.6h-197c-6.4 0-11.6 5.2-11.6 11.6v197c0 6.4 5.2 11.6 11.6 11.6h197c6.4 0 11.6-5.2 11.6-11.7v-197 .1Z" transform="matrix(.03053 0 0 .03046 -3.2 7)"/><path fill="none" stroke="#f1f5f9" stroke-width="65.6" d="M442.5 233.9c0-6.4-5.2-11.6-11.6-11.6h-197c-6.4 0-11.6 5.2-11.6 11.6v197c0 6.4 5.2 11.6 11.6 11.6h197c6.4 0 11.6-5.2 11.6-11.7v-197 .1Z" transform="matrix(.03053 0 0 .03046 6.9 -3.2)"/><path fill="none" stroke="#f1f5f9" stroke-width="70.4" d="M442.5 234.8c0-7-5.6-12.5-12.5-12.5H234.7c-6.8 0-12.4 5.6-12.4 12.5V430c0 6.9 5.6 12.5 12.4 12.5H430c6.9 0 11.5-5.6 12.5-12.5V234.8Z" transform="rotate(45 3.5 24) scale(.02843 .02835)"/></svg>',
	)}`,
}

export function loaderIconUrl(loader?: string | null): string {
	const normalized = (loader || 'fabric').toLowerCase()
	if (normalized.includes('neoforge')) return MODRINTH_LOADER_SVGS.neoforge
	if (normalized.includes('quilt')) return MODRINTH_LOADER_SVGS.quilt
	if (normalized.includes('forge')) return MODRINTH_LOADER_SVGS.forge
	return MODRINTH_LOADER_SVGS.fabric
}

export function formatLoaderName(name?: string | null): string {
	if (!name) return 'Fabric'
	const l = name.toLowerCase()
	if (l.includes('fabric')) return 'Fabric'
	if (l.includes('neoforge')) return 'NeoForge'
	if (l.includes('forge')) return 'Forge'
	if (l.includes('quilt')) return 'Quilt'
	return name
}

const preloadedImageUrls = new Set<string>()

export function preloadCatalogImages(serverList: Dr3shServer[]) {
	if (typeof window === 'undefined') return
	const priorityUrls: string[] = []

	for (const srv of serverList) {
		const logos = [srv.logo_square, srv.logo, srv.image, srv.title_logo]
			.map((u) => resolveImageUrl(u))
			.filter(Boolean)
		for (const u of logos) {
			if (u && !preloadedImageUrls.has(u)) {
				preloadedImageUrls.add(u)
				priorityUrls.push(u)
				queueCacheMediaUrl(u)
			}
		}
		if (srv.id === selectedId.value) {
			const shots = getServerPreviewImages(srv)
			if (shots.length > 0) {
				const stageUrl = getStageImageUrl(shots[0])
				const thumbUrl = getThumbnailUrl(shots[0])
				for (const u of [stageUrl, thumbUrl]) {
					if (u && !preloadedImageUrls.has(u)) {
						preloadedImageUrls.add(u)
						priorityUrls.push(u)
					}
				}
			}
		}
	}

	priorityUrls.forEach((url, idx) => {
		setTimeout(() => {
			const img = new Image()
			img.decoding = 'async'
			img.src = url
		}, idx * 45)
	})
}

export function clearSpecialModesOverride() {
	try {
		localStorage.removeItem('dr3sh.special_modes_override')
	} catch {}
}

export function setSpecialModes(patch: {
	maintenance_mode?: boolean
	maintenance_message?: string
	testing_mode?: boolean
	testing_hours?: number
}) {
	if (!isDeveloper.value) return
	if (typeof patch.maintenance_mode === 'boolean') {
		maintenanceMode.value = patch.maintenance_mode
	}
	if (typeof patch.maintenance_message === 'string') {
		maintenanceMessage.value = patch.maintenance_message
	}
	if (typeof patch.testing_mode === 'boolean') {
		testingMode.value = patch.testing_mode
	}
	if (typeof patch.testing_hours === 'number') {
		testingHours.value = patch.testing_hours
	}
	try {
		localStorage.setItem(
			'dr3sh.special_modes_override',
			JSON.stringify({
				maintenance_mode: maintenanceMode.value,
				maintenance_message: maintenanceMessage.value,
				testing_mode: testingMode.value,
				testing_hours: testingHours.value,
				updated_at: new Date().toISOString(),
			}),
		)
	} catch {}
}

export function applyCatalogData(data: any, persistCache = true, forceApply = false) {
	if (!data || typeof data !== 'object') return

	const incomingRefreshId = String(data.launcher_refresh_id || 'initial')
	// Protect against GitHub CDN returning stale versions.json after a recent publish
	if (
		!forceApply &&
		launcherRefreshId.value &&
		launcherRefreshId.value !== 'initial' &&
		incomingRefreshId !== 'initial' &&
		incomingRefreshId < launcherRefreshId.value
	) {
		return
	}

	const rawServers: Dr3shServer[] = Array.isArray(data.servers) ? data.servers : []
	const rawTopVersions: Dr3shVersion[] = Array.isArray(data.versions) ? data.versions : []

	allCatalogServers.value = rawServers
	allCatalogVersions.value = rawTopVersions

	const refreshId = incomingRefreshId
	launcherRefreshId.value = refreshId
	launcherRefreshMode.value =
		data.launcher_refresh_mode === 'manual_global' || data.launcher_refresh_mode === 'manual_user'
			? data.launcher_refresh_mode
			: 'automatic'

	// Security: Only allow local overrides if the user is an authenticated developer and not forcing remote truth
	let localOverride: any = null
	if (isDeveloper.value && !forceApply) {
		try {
			const rawOverride = localStorage.getItem('dr3sh.special_modes_override')
			if (rawOverride) localOverride = JSON.parse(rawOverride)
		} catch {}
	}

	const isStaleOct24Commit = refreshId === '2026-10-24T06:29:30.407Z' || refreshId === 'initial'
	if (localOverride && (!refreshId || refreshId < String(localOverride.updated_at || ''))) {
		maintenanceMode.value = Boolean(localOverride.maintenance_mode)
		maintenanceMessage.value = String(localOverride.maintenance_message ?? data.maintenance_message ?? '')
		testingMode.value = Boolean(localOverride.testing_mode)
		testingHours.value = Number(localOverride.testing_hours || data.testing_hours || 1)
	} else {
		if (localOverride) {
			clearSpecialModesOverride()
		}
		if (isStaleOct24Commit) {
			maintenanceMode.value = false
			maintenanceMessage.value = String(data.maintenance_message || '')
			testingMode.value = false
			testingHours.value = Number(data.testing_hours || 1)
		} else {
			maintenanceMode.value = Boolean(data.maintenance_mode)
			maintenanceMessage.value = String(data.maintenance_message || '')
			testingMode.value = Boolean(data.testing_mode)
			testingHours.value = Number(data.testing_hours || 1)
		}
	}

	// Normalize servers & attach versions just like Electron's normalizeCatalog()
	const visibleServers = rawServers
		.filter((s) => String(s.status || 'active') !== 'hidden')
		.map((s) => {
			const serverVerList = Array.isArray(s.versions)
				? s.versions
				: rawTopVersions.filter((v) => v.server === s.id)
			const activeVerList = serverVerList.filter((v) => String(v.status || 'active') !== 'hidden')
			return {
				...s,
				screenshots:
					s.id === 'desgraciamon2' && (!Array.isArray(s.screenshots) || s.screenshots.length === 0)
						? DESGRACIAMON2_SCREENSHOTS.map((u) => ({ url: u, status: 'active' }))
						: s.screenshots,
				versions: activeVerList,
			}
		})

	servers.value = visibleServers
	versions.value = rawTopVersions.filter((v) => String(v.status || 'active') !== 'hidden')

	if (selectedId.value && owned.value.size > 0 && servers.value.length > 0 && !servers.value.some((s) => s.id === selectedId.value && owned.value.has(s.id))) {
		const firstOwned = servers.value.find((s) => owned.value.has(s.id))
		if (firstOwned) {
			selectedId.value = firstOwned.id
		}
	}
	updateThemePalette(servers.value.find((s) => s.id === selectedId.value))
	preloadCatalogImages(visibleServers)

	if (persistCache) {
		try {
			localStorage.setItem('dr3sh.cached_catalog_v2', JSON.stringify(data))
		} catch {}
	}
}

// Hydrate immediately from cached catalog if available so startup is 0ms
try {
	const cachedRaw = localStorage.getItem('dr3sh.cached_catalog_v2')
	if (cachedRaw) {
		const parsed = JSON.parse(cachedRaw)
		if (parsed && Array.isArray(parsed.servers) && parsed.servers.length > 0) {
			applyCatalogData(parsed, false)
			loading.value = false
		}
	}
} catch {}

async function fetchJsonWithTimeout(
	url: string,
	headers: Record<string, string>,
	timeoutMs = 8000,
): Promise<any | null> {
	const controller = new AbortController()
	const timer = setTimeout(() => controller.abort(), timeoutMs)
	try {
		const res = await tauriFetch(url, {
			headers,
			signal: controller.signal,
		})
		if (!res || !res.ok) return null
		return await res.json().catch(() => null)
	} catch {
		return null
	} finally {
		clearTimeout(timer)
	}
}

export async function fetchVersionsData(forceLive = false) {
	retryingCatalog.value = true
	try {
		if (servers.value.length === 0) {
			loading.value = true
		}
		// Fetch both live release asset (instant, 0 CDN delay) and raw repository in parallel
		const liveUrl = `https://github.com/Dr3sh/modpack-versions/releases/download/catalog-live/versions.json?t=${Date.now()}`
		const fallbackUrl = `https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/versions.json?t=${Date.now()}`

		const [liveData, repoData] = await Promise.all([
			fetchJsonWithTimeout(liveUrl, {
				'Cache-Control': 'no-cache, no-store, must-revalidate',
			}),
			fetchJsonWithTimeout(fallbackUrl, {
				'Cache-Control': 'no-cache',
			}),
		])

		let chosenData = liveData || repoData
		if (liveData && repoData) {
			const liveTime = new Date(liveData.launcher_refresh_id || 0).getTime()
			const repoTime = new Date(repoData.launcher_refresh_id || 0).getTime()
			// If manual edit in github repo is newer, respect it; otherwise use fast live release asset
			if (repoTime > liveTime) {
				chosenData = repoData
			} else {
				chosenData = liveData
			}
		}

		if (chosenData) {
			catalogError.value = null
			isOffline.value = false
			startupBlockedOffline.value = false
			offlineSinceMs.value = null
			connectionLostFatal.value = false
			applyCatalogData(chosenData, true, forceLive)
			void syncNetworkClock()
		} else {
			catalogError.value =
				'No se pudo conectar con los servidores de Dr3sh. Comprueba tu conexión a internet.'
			if (typeof navigator !== 'undefined' && !navigator.onLine) {
				isOffline.value = true
			}
			if (!startupChecked.value && servers.value.length === 0) {
				startupBlockedOffline.value = true
			}
		}
		startupChecked.value = true
	} catch (err) {
		console.error('Failed to load Dr3sh versions:', err)
		catalogError.value =
			'No se pudo conectar con los servidores de Dr3sh. Comprueba tu conexión a internet.'
		if (!startupChecked.value && servers.value.length === 0) {
			startupBlockedOffline.value = true
		}
		startupChecked.value = true
	} finally {
		loading.value = false
		retryingCatalog.value = false
	}
}

export const NETWORK_LOSS_GRACE_MS = 90_000
export const startupChecked = ref(false)
export const startupBlockedOffline = ref(false)
export const connectionLostFatal = ref(false)
export const hasActiveGameRunning = ref(false)
export const offlineSinceMs = ref<number | null>(null)
export const networkClockOffsetMs = ref(0)

export interface DebugPreviewState {
	open: boolean
	forceMaintenance: boolean
	forceNetworkState: 'normal' | 'startup_offline' | 'lost_connection' | 'catalog_error'
	forceButtonAction:
		| 'auto'
		| 'download'
		| 'preload'
		| 'preloaded'
		| 'countdown'
		| 'play'
		| 'update'
		| 'repair'
		| 'running'
		| 'disabled'
	viewportPreset: 'auto' | 'compact' | 'standard' | 'wide'
}

export const debugPreview = ref<DebugPreviewState>({
	open: false,
	forceMaintenance: false,
	forceNetworkState: 'normal',
	forceButtonAction: 'auto',
	viewportPreset: 'auto',
})

export function resetDebugPreview() {
	debugPreview.value = {
		open: false,
		forceMaintenance: false,
		forceNetworkState: 'normal',
		forceButtonAction: 'auto',
		viewportPreset: 'auto',
	}
}

export function parseReleaseDateMs(raw?: string): number {
	if (!raw) return NaN
	const trimmed = raw.trim()
	if (!trimmed) return NaN
	// If datetime-local has no timezone offset, interpret as Venezuela UTC-4 (-04:00)
	if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/.test(trimmed)) {
		return new Date(`${trimmed}-04:00`).getTime()
	}
	return new Date(trimmed).getTime()
}

export function getRealNowMs(): number {
	return Date.now() + networkClockOffsetMs.value
}

export async function syncNetworkClock() {
	try {
		const netMs = await invoke<number | null>('dr3sh_get_network_time')
		if (typeof netMs === 'number' && Number.isFinite(netMs) && netMs > 0) {
			networkClockOffsetMs.value = netMs - Date.now()
		}
	} catch {
		// Ignore network clock sync failure
	}
}

export async function checkNetworkHealthTick() {
	if (typeof navigator !== 'undefined' && navigator.onLine) {
		try {
			const netMs = await invoke<number | null>('dr3sh_get_network_time')
			if (typeof netMs === 'number' && Number.isFinite(netMs) && netMs > 0) {
				networkClockOffsetMs.value = netMs - Date.now()
				offlineSinceMs.value = null
				isOffline.value = false
				return
			}
		} catch {
			// Fall through to offline tracking
		}
	}

	if (hasActiveGameRunning.value) {
		// Never interrupt or demand restart while Minecraft is open
		offlineSinceMs.value = null
		return
	}

	const now = Date.now()
	if (offlineSinceMs.value === null) {
		offlineSinceMs.value = now
	} else if (now - offlineSinceMs.value >= NETWORK_LOSS_GRACE_MS) {
		isOffline.value = true
		connectionLostFatal.value = true
	}
}

// Shared modal trigger for deleting local install or removing modpack from sidebar or main page
export interface DeleteConfirmRequest {
	server: Dr3shServer
	mode: 'local-only' | 'pack-remove'
	secondsLeft: number
}

export const deleteConfirmModal = ref<DeleteConfirmRequest | null>(null)
export const pendingInstallServer = ref<Dr3shServer | null>(null)
let deleteCountdownTimer: ReturnType<typeof setInterval> | null = null

export function requestInstallModpack(server: Dr3shServer) {
	pendingInstallServer.value = server
}

export function requestDeleteConfirm(server: Dr3shServer, mode: 'local-only' | 'pack-remove') {
	if (deleteCountdownTimer) {
		clearInterval(deleteCountdownTimer)
		deleteCountdownTimer = null
	}
	deleteConfirmModal.value = {
		server,
		mode,
		secondsLeft: 3,
	}
	deleteCountdownTimer = setInterval(() => {
		if (!deleteConfirmModal.value) {
			if (deleteCountdownTimer) clearInterval(deleteCountdownTimer)
			return
		}
		if (deleteConfirmModal.value.secondsLeft > 0) {
			deleteConfirmModal.value.secondsLeft -= 1
		} else if (deleteCountdownTimer) {
			clearInterval(deleteCountdownTimer)
			deleteCountdownTimer = null
		}
	}, 1000)
}

export function closeDeleteConfirm() {
	if (deleteCountdownTimer) {
		clearInterval(deleteCountdownTimer)
		deleteCountdownTimer = null
	}
	deleteConfirmModal.value = null
}

export function useDr3shLauncher() {
	const selectedServer = computed(() => {
		if (!selectedId.value || owned.value.size === 0) return null
		return servers.value.find((s) => s.id === selectedId.value && owned.value.has(s.id)) || null
	})

	const selectedVersion = computed(() => {
		if (!selectedServer.value) return null
		if (Array.isArray(selectedServer.value.versions) && selectedServer.value.versions.length > 0) {
			return selectedServer.value.versions[0]
		}
		return versions.value.find((v) => v.server === selectedServer.value.id) || null
	})

	const ownedServers = computed(() => {
		return servers.value.filter((s) => owned.value.has(s.id))
	})

	watch(
		selectedServer,
		(srv) => {
			updateThemePalette(srv)
		},
		{ immediate: true },
	)

	function addOwned(id: string) {
		const next = new Set(owned.value)
		next.add(id)
		owned.value = next
		persistOwned()
		selectedId.value = id
		currentView.value = 'home'
	}

	function removeOwned(id: string) {
		const next = new Set(owned.value)
		next.delete(id)
		owned.value = next
		persistOwned()
		if (selectedId.value === id) {
			const remaining = Array.from(owned.value)
			if (remaining.length > 0) {
				selectedId.value = remaining[0]
			}
		}
	}

	return {
		launcherVersion,
		servers,
		versions,
		allCatalogServers,
		allCatalogVersions,
		loading,
		owned,
		ownedServers,
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
		isOffline,
		catalogError,
		retryingCatalog,
		launcherRefreshMode,
		launcherRefreshId,
		deleteConfirmModal,
		pendingInstallServer,
		requestInstallModpack,
		requestDeleteConfirm,
		closeDeleteConfirm,
		refreshAccount,
		loginOfflineAccount,
		loginMicrosoftAccount,
		switchActiveAccount,
		removeSavedAccount,
		fetchVersionsData,
		applyCatalogData,
		setSpecialModes,
		clearSpecialModesOverride,
		addOwned,
		removeOwned,
		resolveImageUrl,
		handleAvatarError,
		getStageImageUrl,
		getThumbnailUrl,
		handleImageError,
		getServerPreviewImages,
		hasCustomScreenshots,
		loaderIconUrl,
		formatLoaderName,
		updateThemePalette,
		getAccentColor,
		getContrastTextColor,
		mixHex,
		isSidebarCollapsed,
		railWidth,
		setRailWidth,
		toggleSidebarCollapse,
		deselectModpack,
		startupChecked,
		startupBlockedOffline,
		connectionLostFatal,
		hasActiveGameRunning,
		debugPreview,
		resetDebugPreview,
		parseReleaseDateMs,
		getRealNowMs,
		syncNetworkClock,
		checkNetworkHealthTick,
	}
}
