import { computed, ref, watch } from 'vue'
import { fetch as tauriFetch } from '@tauri-apps/plugin-http'
import {
	add_offline_user,
	get_default_user,
	login as tauriMicrosoftLogin,
	remove_user,
	set_default_user,
	users,
} from '@/helpers/auth'

export type ScreenshotEntry = string | { url: string; status?: string; [key: string]: unknown }

export interface Dr3shServer {
	id: string
	title: string
	aliases?: string[]
	status?: string
	image?: string
	logo?: string
	logo_square?: string
	theme_color?: string
	description?: string
	minecraft_version?: string
	mod_loader?: string
	mod_loader_version?: string
	screenshots?: ScreenshotEntry[]
	server_address?: string
	versions?: Dr3shVersion[]
}

export interface Dr3shVersion {
	id: string
	server: string
	title: string
	version?: string
	description?: string
	link?: string
	manifest?: string
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

const servers = ref<Dr3shServer[]>([])
const versions = ref<Dr3shVersion[]>([])
const allCatalogServers = ref<Dr3shServer[]>([])
const allCatalogVersions = ref<Dr3shVersion[]>([])
const loading = ref(true)

export const maintenanceMode = ref(false)
export const maintenanceMessage = ref('')
export const testingMode = ref(false)
export const testingHours = ref(1)
export const launcherRefreshMode = ref<'automatic' | 'manual_global' | 'manual_user'>('automatic')
export const launcherRefreshId = ref('initial')
export const isDeveloper = computed(() => {
	try {
		const token = (
			localStorage.getItem('dr3sh.developer_token') ||
			localStorage.getItem('dr3sh.github_token') ||
			''
		).trim()
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

// Persisted owned servers - default to the 4 instances from original Electron launcher
function loadInitialOwned(): Set<string> {
	try {
		const raw = localStorage.getItem('dr3sh_owned_servers') || localStorage.getItem('dr3sh.owned')
		if (raw) {
			const arr = JSON.parse(raw)
			if (Array.isArray(arr) && arr.length > 0) return new Set(arr)
		}
	} catch {}
	return new Set(['opus', 'cobbleverse', 'desgraciamon2', 'desgraciamon3'])
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

export const selectedId = ref<string>(localStorage.getItem('dr3sh_selected_server') || 'desgraciamon3')

watch(selectedId, (val) => {
	if (val) localStorage.setItem('dr3sh_selected_server', val)
})

export const currentView = ref<'home' | 'market' | 'settings'>('home')
export const activeSettingsTab = ref<'memory' | 'account' | 'game' | 'storage' | 'about'>('memory')

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
	if (!server) return '#2f65f6'
	const raw = (server as any).theme_color || (server as any).color
	if (raw && /^#[\da-f]{6}$/i.test(String(raw).trim())) return String(raw).trim()
	if (server.id === 'desgraciamon3') return '#2f65f6'
	if (server.id === 'opus') return '#b01212'
	if (server.id === 'cobbleverse') return '#4b5563'
	if (server.id === 'desgraciamon2') return '#ec4899'
	return '#2f65f6'
}

export function getAccentColor(server?: Dr3shServer | null): string {
	const raw = getRawServerColor(server)
	const [h, s, l] = hexToHsl(raw)
	// Harmonize modpack color with launcher's dark-slate aesthetic so neither ultra-dark (#0c1645),
	// neutral gray (#383838), nor pastel (#f6a0d3) clashes with the UI.
	if (s < 0.12) {
		return mixHex(raw, '#4f7df3', 0.55)
	}
	const clampedS = Math.max(0.48, Math.min(0.76, s))
	const clampedL = Math.max(0.48, Math.min(0.6, l))
	const normalized = hslToHex(h, clampedS, clampedL)
	return mixHex(normalized, '#3b6df6', 0.16)
}

const INITIAL_COLORS = {
	'--ink': '#0c1018',
	'--panel': '#131824',
	'--panel-2': '#1a2130',
	'--line': '#283246',
}

export function updateThemePalette(server?: Dr3shServer | null) {
	if (typeof document === 'undefined') return
	const rawColor = getRawServerColor(server)
	const brandAccent = getAccentColor(server)
	const surfaceTint = mixHex(rawColor, brandAccent, 0.5)
	const root = document.documentElement
	root.style.setProperty('--ink', mixHex(INITIAL_COLORS['--ink'], surfaceTint, 0.12))
	root.style.setProperty('--panel', mixHex(INITIAL_COLORS['--panel'], surfaceTint, 0.18))
	root.style.setProperty('--panel-2', mixHex(INITIAL_COLORS['--panel-2'], surfaceTint, 0.24))
	root.style.setProperty('--line', mixHex(INITIAL_COLORS['--line'], brandAccent, 0.36))
	root.style.setProperty('--brand', brandAccent)
	root.style.setProperty('--brand-hover', mixHex(brandAccent, '#ffffff', 0.16))
	root.style.setProperty('--brand-soft', mixHex('#0c1018', brandAccent, 0.38))
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

export function getStageImageUrl(url?: string | null): string {
	const resolved = resolveImageUrl(url)
	if (!resolved) return '/logo.png'
	// Compress 3MB+ uncompressed PNG uploads via global WebP edge cache for instant display
	if (resolved.includes('raw.githubusercontent.com') && resolved.includes('/assets/uploads/') && /\.png$/i.test(resolved)) {
		return `https://wsrv.nl/?url=${encodeURIComponent(resolved)}&w=1280&q=85&output=webp`
	}
	return resolved
}

export function getThumbnailUrl(url?: string | null): string {
	const resolved = resolveImageUrl(url)
	if (!resolved) return '/logo.png'
	if (resolved.includes('raw.githubusercontent.com') && (resolved.includes('/assets/uploads/') || resolved.includes('/assets/screenshots/')) && /\.(png|jpg|jpeg)$/i.test(resolved)) {
		return `https://wsrv.nl/?url=${encodeURIComponent(resolved)}&w=260&q=80&output=webp`
	}
	return resolved
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
	return fallback.length > 0 ? [fallback[0]] : ['/logo.png']
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
	const secondaryUrls: string[] = []

	for (const srv of serverList) {
		const logos = [srv.logo_square, srv.logo, srv.image]
			.map((u) => resolveImageUrl(u))
			.filter(Boolean)
		const shots = getServerPreviewImages(srv)
		for (const u of logos) {
			if (u && !preloadedImageUrls.has(u)) {
				preloadedImageUrls.add(u)
				priorityUrls.push(u)
			}
		}
		shots.forEach((rawShot, idx) => {
			const stageUrl = getStageImageUrl(rawShot)
			const thumbUrl = getThumbnailUrl(rawShot)
			const list = srv.id === selectedId.value || idx === 0 ? priorityUrls : secondaryUrls
			for (const u of [stageUrl, thumbUrl]) {
				if (u && !preloadedImageUrls.has(u)) {
					preloadedImageUrls.add(u)
					list.push(u)
				}
			}
		})
	}

	// Preload priority assets (logos + selected server screenshots) first, then background assets gently
	priorityUrls.forEach((url, idx) => {
		setTimeout(() => {
			const img = new Image()
			img.decoding = 'async'
			img.src = url
		}, idx * 35)
	})
	secondaryUrls.forEach((url, idx) => {
		setTimeout(() => {
			const img = new Image()
			img.decoding = 'async'
			img.src = url
		}, 600 + idx * 120)
	})
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

export function applyCatalogData(data: any, persistCache = true) {
	if (!data || typeof data !== 'object') return
	const rawServers: Dr3shServer[] = Array.isArray(data.servers) ? data.servers : []
	const rawTopVersions: Dr3shVersion[] = Array.isArray(data.versions) ? data.versions : []

	allCatalogServers.value = rawServers
	allCatalogVersions.value = rawTopVersions

	const refreshId = String(data.launcher_refresh_id || 'initial')
	launcherRefreshId.value = refreshId
	launcherRefreshMode.value =
		data.launcher_refresh_mode === 'manual_global' || data.launcher_refresh_mode === 'manual_user'
			? data.launcher_refresh_mode
			: 'automatic'

	// Security: Only allow local overrides if the user is an authenticated developer
	let localOverride: any = null
	if (isDeveloper.value) {
		try {
			const rawOverride = localStorage.getItem('dr3sh.special_modes_override')
			if (rawOverride) localOverride = JSON.parse(rawOverride)
		} catch {}
	}

	const isStaleOct24Commit = refreshId === '2026-10-24T06:29:30.407Z' || refreshId === 'initial'
	if (localOverride && (!refreshId || refreshId <= String(localOverride.updated_at || ''))) {
		maintenanceMode.value = Boolean(localOverride.maintenance_mode)
		maintenanceMessage.value = String(localOverride.maintenance_message ?? data.maintenance_message ?? '')
		testingMode.value = Boolean(localOverride.testing_mode)
		testingHours.value = Number(localOverride.testing_hours || data.testing_hours || 1)
	} else if (isStaleOct24Commit) {
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

	if (servers.value.length > 0 && !servers.value.some((s) => s.id === selectedId.value)) {
		const defaultFound = servers.value.find((s) => s.id === 'desgraciamon3')
		selectedId.value = defaultFound ? defaultFound.id : servers.value[0].id
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

export async function fetchVersionsData() {
	try {
		if (servers.value.length === 0) {
			loading.value = true
		}
		const url = `https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/versions.json?t=${Date.now()}`
		const res = await tauriFetch(url, {
			headers: {
				'Cache-Control': 'no-cache',
			},
		})
		if (res.ok) {
			const data = await res.json()
			applyCatalogData(data, true)
		}
	} catch (err) {
		console.error('Failed to load Dr3sh versions:', err)
	} finally {
		loading.value = false
	}
}

// Shared modal trigger for deleting local install or removing modpack from sidebar or main page
export interface DeleteConfirmRequest {
	server: Dr3shServer
	mode: 'local-only' | 'pack-remove'
	secondsLeft: number
}

export const deleteConfirmModal = ref<DeleteConfirmRequest | null>(null)
let deleteCountdownTimer: ReturnType<typeof setInterval> | null = null

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
		return servers.value.find((s) => s.id === selectedId.value) || servers.value[0] || null
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
		launcherRefreshMode,
		launcherRefreshId,
		deleteConfirmModal,
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
		addOwned,
		removeOwned,
		resolveImageUrl,
		getStageImageUrl,
		getThumbnailUrl,
		handleImageError,
		getServerPreviewImages,
		hasCustomScreenshots,
		loaderIconUrl,
		formatLoaderName,
		updateThemePalette,
		getAccentColor,
		mixHex,
	}
}
