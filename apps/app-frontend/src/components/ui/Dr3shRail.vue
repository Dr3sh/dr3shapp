<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useQuery } from '@tanstack/vue-query'
import { instanceListQueryOptions } from '@/pages/instance/query-options'
import {
	useDr3shLauncher,
	type Dr3shServer,
	getServerPreviewImages,
	getThumbnailUrl,
	handleImageError,
	loaderIconUrl,
	formatLoaderName,
	requestDeleteConfirm,
} from '@/composables/use-dr3sh-launcher'
import {
	installedVersionsMap,
	getInstalledVersion,
	findMatchingInstance as findMatchingInstanceHelper,
} from '@/composables/use-dr3sh-downloader'
import { install_existing_instance } from '@/helpers/install'
import { showInstanceInFolder } from '@/helpers/utils.js'

const DR3SH_LOGO_DATA_URI = '/logo.png'

const router = useRouter()
const {
	ownedServers,
	versions,
	selectedId,
	currentView,
	activeSettingsTab,
	activeAccount,
	refreshAccount,
	resolveImageUrl,
} = useDr3shLauncher()

const instancesQuery = useQuery(instanceListQueryOptions())
const instances = computed(() => instancesQuery.data.value ?? [])

function findMatchingInstance(server: Dr3shServer) {
	return findMatchingInstanceHelper(server, instances.value)
}

function isServerInstalled(server: Dr3shServer): boolean {
	// Touch reactive map
	const _map = installedVersionsMap.value
	const hasVer = Boolean(getInstalledVersion(server.id))
	const hasInst = Boolean(findMatchingInstance(server))
	return hasInst && hasVer
}

function hasServerUpdate(server: Dr3shServer): boolean {
	if (!isServerInstalled(server)) return false
	const installedVer = getInstalledVersion(server.id)
	const latestObj =
		Array.isArray(server.versions) && server.versions.length > 0
			? server.versions[0]
			: versions.value.find((v) => v.server === server.id)
	if (!installedVer || !latestObj?.version) return false
	const cleanInstalled = installedVer.replace(/^v/i, '').trim()
	const cleanLatest = latestObj.version.replace(/^v/i, '').trim()
	return cleanInstalled !== cleanLatest
}

function formatSubtitle(server: Dr3shServer): string {
	const loader = formatLoaderName(server.mod_loader)
	const mc = server.minecraft_version || '1.21.1'
	const installed = isServerInstalled(server)
	return `${loader} - ${mc} - ${installed ? 'Instalado' : 'Sin instalar'}`
}

// Sidebar Hover Screenshot Preview state
const hoverPreview = ref<{ server: Dr3shServer; top: number; left: number } | null>(null)
const hoverPreviewIdx = ref(0)
let hoverTimer: ReturnType<typeof setInterval> | null = null

const hoverPreviewImages = computed(() => {
	if (!hoverPreview.value) return []
	return getServerPreviewImages(hoverPreview.value.server)
})

watch(hoverPreview, (val) => {
	hoverPreviewIdx.value = 0
	if (hoverTimer) {
		clearInterval(hoverTimer)
		hoverTimer = null
	}
	if (val && hoverPreviewImages.value.length > 1) {
		hoverTimer = setInterval(() => {
			const total = hoverPreviewImages.value.length
			if (total > 1) {
				hoverPreviewIdx.value = (hoverPreviewIdx.value + 1) % total
			}
		}, 1600)
	}
})

function onServerMouseEnter(event: MouseEvent, server: Dr3shServer) {
	const target = event.currentTarget as HTMLElement
	if (!target) return
	const rect = target.getBoundingClientRect()
	hoverPreview.value = {
		server,
		top: Math.min(Math.max(12, rect.top - 16), window.innerHeight - 160),
		left: rect.right + 12,
	}
}

function onServerMouseLeave(serverId: string) {
	if (hoverPreview.value?.server.id === serverId) {
		hoverPreview.value = null
	}
}

// Sidebar Right-Click Context Menu state
const contextMenu = ref<{ server: Dr3shServer; x: number; y: number } | null>(null)

function openSidebarContextMenu(event: MouseEvent, server: Dr3shServer) {
	event.preventDefault()
	event.stopPropagation()
	selectedId.value = server.id
	hoverPreview.value = null
	contextMenu.value = {
		server,
		x: Math.min(event.clientX, window.innerWidth - 264),
		y: Math.min(event.clientY, window.innerHeight - 230),
	}
}

function closeContextMenu() {
	contextMenu.value = null
}

async function handleContextOpenFolder(server: Dr3shServer) {
	closeContextMenu()
	const inst = findMatchingInstance(server)
	if (inst) {
		try {
			await showInstanceInFolder(inst.id)
		} catch (e) {
			console.warn('Error opening folder:', e)
		}
	}
}

async function handleContextRepair(server: Dr3shServer) {
	closeContextMenu()
	const inst = findMatchingInstance(server)
	if (inst) {
		try {
			await install_existing_instance(inst.id, true)
			await instancesQuery.refetch()
		} catch (e) {
			console.warn('Error repairing instance:', e)
		}
	}
}

function handleContextDeleteLocal(server: Dr3shServer) {
	closeContextMenu()
	requestDeleteConfirm(server, 'local-only')
}

function handleContextRemovePack(server: Dr3shServer) {
	closeContextMenu()
	requestDeleteConfirm(server, 'pack-remove')
}

onMounted(() => {
	refreshAccount()
	window.addEventListener('click', closeContextMenu)
})

onUnmounted(() => {
	window.removeEventListener('click', closeContextMenu)
	if (hoverTimer) clearInterval(hoverTimer)
})

function selectServer(id: string) {
	selectedId.value = id
	currentView.value = 'home'
	if (router.currentRoute.value.path !== '/') {
		router.push('/')
	}
}

function navigateTo(view: 'home' | 'market' | 'settings') {
	currentView.value = view
	if (router.currentRoute.value.path !== '/') {
		router.push('/')
	}
}

function onSettingsClick() {
	currentView.value = 'settings'
	if (router.currentRoute.value.path !== '/') {
		router.push('/')
	}
}

function onProfileClick() {
	activeSettingsTab.value = 'account'
	currentView.value = 'settings'
	if (router.currentRoute.value.path !== '/') {
		router.push('/')
	}
}

const userAvatarUrl = computed(() => {
	if (activeAccount.value?.uuid && activeAccount.value?.isMicrosoft) {
		return `https://mc-heads.net/avatar/${activeAccount.value.uuid}/64`
	}
	if (activeAccount.value?.name) {
		return `https://mc-heads.net/avatar/${encodeURIComponent(activeAccount.value.name)}/64`
	}
	return 'https://mc-heads.net/avatar/fkdr3sh/64'
})
</script>

<template>
	<aside class="rail">
		<!-- Brand Lockup -->
		<div
			class="brand-lockup"
			role="button"
			tabindex="0"
			title="Dr3sh Launcher v0.2.16"
			@click="navigateTo('home')"
		>
			<img :src="DR3SH_LOGO_DATA_URI" alt="Dr3sh" class="brand-logo-img" />
			<span class="brand-version-badge">v0.2.16</span>
		</div>

		<!-- Section Head: INSTANCIAS -->
		<div class="rail-section-head">
			<span>INSTANCIAS</span>
			<span class="rail-count">{{ ownedServers.length }}</span>
		</div>

		<!-- Instances List -->
		<nav class="server-list" aria-label="Biblioteca de modpacks">
			<button
				v-for="server in ownedServers"
				:key="server.id"
				type="button"
				class="server-nav"
				:class="{ active: selectedId === server.id && currentView === 'home' }"
				@click="selectServer(server.id)"
				@contextmenu.prevent="openSidebarContextMenu($event, server)"
				@mouseenter="onServerMouseEnter($event, server)"
				@mouseleave="onServerMouseLeave(server.id)"
			>
				<span class="server-thumb">
					<img
						v-if="server.logo_square || server.logo || server.image"
						:src="resolveImageUrl(server.logo_square || server.logo || server.image)"
						:alt="server.title"
					/>
					<span v-else class="w-full h-full flex items-center justify-center font-bold text-xs text-white/50">
						{{ server.title.slice(0, 2) }}
					</span>
				</span>
				<span class="server-nav-copy">
					<span class="server-nav-title">{{ server.title }}</span>
					<span class="server-nav-sub" :title="formatSubtitle(server)">
						<img :src="loaderIconUrl(server.mod_loader)" alt="" class="loader-logo-img" />
						<span>{{ formatSubtitle(server) }}</span>
					</span>
				</span>
				<span
					v-if="hasServerUpdate(server)"
					class="update-badge"
					title="Actualización disponible"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M10 15V5m0 0-4 4m4-4 4 4" />
					</svg>
				</span>
			</button>

			<div v-if="ownedServers.length === 0" class="empty-library">
				<p>No tienes instancias en tu biblioteca.</p>
				<button type="button" class="empty-library-btn" @click="navigateTo('market')">
					Ir a Explorar
				</button>
			</div>
		</nav>

		<!-- Bottom Section -->
		<div class="rail-bottom">
			<div class="rail-nav-buttons">
				<button
					type="button"
					class="rail-link"
					:class="{ selected: currentView === 'home' }"
					title="Ver biblioteca de instancias"
					@click="navigateTo('home')"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M3 8.5 10 3l7 5.5v7.5a1 1 0 0 1-1 1h-4v-5H8v5H4a1 1 0 0 1-1-1V8.5Z" />
					</svg>
					<span>Biblioteca</span>
				</button>
				<button
					type="button"
					class="rail-link"
					:class="{ selected: currentView === 'market' }"
					title="Explorar modpacks disponibles"
					@click="navigateTo('market')"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<circle cx="10" cy="10" r="7" />
						<path d="m12.8 7.2-1.8 4.6-4.6 1.8 1.8-4.6 4.6-1.8Z" />
					</svg>
					<span>Explorar</span>
				</button>
				<button
					type="button"
					class="rail-link"
					:class="{ selected: currentView === 'settings' }"
					title="Abrir ajustes del launcher"
					@click="onSettingsClick"
				>
					<svg viewBox="0 0 24 24" aria-hidden="true">
						<path
							d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"
						/>
						<circle cx="12" cy="12" r="3" />
					</svg>
					<span>Ajustes</span>
				</button>
			</div>

			<!-- Profile Line -->
			<button
				type="button"
				class="profile-line"
				title="Gestionar cuentas de Minecraft"
				@click="onProfileClick"
			>
				<span class="profile-avatar">
					<img :src="userAvatarUrl" alt="" class="skin-face-img" />
				</span>
				<span class="profile-copy">
					<b>{{ activeAccount?.name || 'fkdr3sh' }}</b>
					<small>{{ activeAccount?.isMicrosoft ? 'Microsoft' : 'Sin conexión' }}</small>
				</span>
				<span class="account-pip online" />
			</button>
		</div>

		<!-- Hover Screenshot Preview Floater -->
		<Teleport to="body">
			<div
				v-if="hoverPreview && hoverPreviewImages.length > 0"
				class="sidebar-hover-preview-floater"
				:style="{ top: `${hoverPreview.top}px`, left: `${hoverPreview.left}px` }"
			>
				<div class="library-preview-slideshow sidebar" aria-hidden="true">
					<img
						:src="getThumbnailUrl(hoverPreviewImages[hoverPreviewIdx])"
						alt=""
						class="library-preview-img"
						@error="handleImageError($event, hoverPreviewImages[hoverPreviewIdx])"
					/>
					<div class="library-preview-overlay" />
					<div v-if="hoverPreviewImages.length > 1" class="library-preview-dots">
						<span
							v-for="(_, idx) in hoverPreviewImages.slice(0, 5)"
							:key="idx"
							class="library-preview-dot"
							:class="{ active: idx === hoverPreviewIdx % Math.min(5, hoverPreviewImages.length) }"
						/>
					</div>
				</div>
			</div>

			<!-- Sidebar Right-Click Context Menu -->
			<div
				v-if="contextMenu"
				class="instance-dropdown-menu context-floating"
				role="menu"
				:style="{ top: `${contextMenu.y}px`, left: `${contextMenu.x}px` }"
				@click.stop
			>
				<div class="instance-menu-header">
					<span class="instance-menu-header-icon">
						<img
							v-if="contextMenu.server.logo_square || contextMenu.server.logo || contextMenu.server.image"
							:src="resolveImageUrl(contextMenu.server.logo_square || contextMenu.server.logo || contextMenu.server.image)"
							:alt="contextMenu.server.title"
						/>
					</span>
					<div class="instance-menu-header-copy">
						<strong>{{ contextMenu.server.title }}</strong>
						<small>{{ formatSubtitle(contextMenu.server) }}</small>
					</div>
				</div>
				<button
					type="button"
					class="instance-menu-item primary-item"
					@click="selectServer(contextMenu.server.id); closeContextMenu()"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M6.5 4.5v11l9-5.5-9-5.5Z" fill="currentColor" stroke="none" />
					</svg>
					<span>Abrir instancia</span>
				</button>
				<div class="instance-menu-divider" />
				<button
					type="button"
					class="instance-menu-item"
					@click="handleContextRepair(contextMenu.server)"
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
					@click="handleContextOpenFolder(contextMenu.server)"
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
					:disabled="!isServerInstalled(contextMenu.server)"
					@click="handleContextDeleteLocal(contextMenu.server)"
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
					@click="handleContextRemovePack(contextMenu.server)"
				>
					<svg viewBox="0 0 20 20" aria-hidden="true">
						<path d="M3 5h14M8 5V3h4v2m-6 3 1 9h6l1-9M7 8h6" />
					</svg>
					<span>Quitar modpack del launcher</span>
				</button>
			</div>
		</Teleport>
	</aside>
</template>
