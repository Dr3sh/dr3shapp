<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useQuery } from '@tanstack/vue-query'
import { invoke } from '@tauri-apps/api/core'
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
	hasServerOrRuntimeUpdate,
	findMatchingInstance as findMatchingInstanceHelper,
} from '@/composables/use-dr3sh-downloader'
import { run } from '@/helpers/instance'
import { install_existing_instance } from '@/helpers/install'
import { showInstanceInFolder } from '@/helpers/utils.js'

const DR3SH_LOGO_DATA_URI = '/logo.png'

const router = useRouter()
const {
	launcherVersion,
	ownedServers,
	versions,
	selectedId,
	currentView,
	activeSettingsTab,
	activeAccount,
	isDeveloper,
	refreshAccount,
	resolveImageUrl,
	isSidebarCollapsed,
	railWidth,
	setRailWidth,
	toggleSidebarCollapse,
	deselectModpack,
	handleAvatarError,
} = useDr3shLauncher()

const instancesQuery = useQuery(instanceListQueryOptions())
const instances = computed(() => instancesQuery.data.value ?? [])

function findMatchingInstance(server: Dr3shServer) {
	return findMatchingInstanceHelper(server, instances.value)
}

function isServerInstalled(server: Dr3shServer): boolean {
	const _map = installedVersionsMap.value
	const hasVer = Boolean(getInstalledVersion(server.id))
	const hasInst = Boolean(findMatchingInstance(server))
	return hasInst && hasVer
}

function hasServerUpdate(server: Dr3shServer): boolean {
	const _map = installedVersionsMap.value
	if (!isServerInstalled(server)) return false
	const latestObj =
		Array.isArray(server.versions) && server.versions.length > 0
			? server.versions[0]
			: versions.value.find((v) => v.server === server.id)
	return hasServerOrRuntimeUpdate(server, latestObj, findMatchingInstance(server))
}

function formatSubtitle(server: Dr3shServer): string {
	const loader = formatLoaderName(server.mod_loader)
	const mc = server.minecraft_version || '1.21.1'
	return `${loader} • ${mc}`
}

function selectServer(id: string) {
	selectedId.value = id
	currentView.value = 'home'
	if (router.currentRoute.value.path !== '/') {
		router.push('/')
	}
}

async function handleQuickPlay(event: MouseEvent, server: Dr3shServer) {
	event.stopPropagation()
	selectServer(server.id)
	const inst = findMatchingInstance(server)
	if (inst && isServerInstalled(server)) {
		try {
			if (!activeAccount.value) {
				await refreshAccount()
			}
			await invoke('dr3sh_prepare_launch', {
				serverId: server.id,
				isOffline: !activeAccount.value?.isMicrosoft,
				offlineUsername: activeAccount.value?.name || 'Player',
				bypassGate: Boolean(isDeveloper.value),
			})
			await run(inst.id)
		} catch (e) {
			console.warn('Quick play failed:', e)
		}
	}
}

function navigateTo(view: 'home' | 'market' | 'settings') {
	currentView.value = view
	if (router.currentRoute.value.path !== '/') {
		router.push('/')
	}
}

function onSettingsClick() {
	activeSettingsTab.value = 'memory'
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
	return 'https://mc-heads.net/avatar/Player/64'
})

// Right-click context menu
const contextMenu = ref<{ server: Dr3shServer; x: number; y: number } | null>(null)
function openSidebarContextMenu(event: MouseEvent, server: Dr3shServer) {
	event.preventDefault()
	event.stopPropagation()
	selectedId.value = server.id
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

// Resizer drag handler
let isResizing = false
function onResizerMouseDown(e: MouseEvent) {
	e.preventDefault()
	isResizing = true
	document.body.classList.add('is-resizing-rail')
	window.addEventListener('mousemove', onResizerMouseMove)
	window.addEventListener('mouseup', onResizerMouseUp)
}
function onResizerMouseMove(e: MouseEvent) {
	if (!isResizing) return
	setRailWidth(e.clientX, true)
}
function onResizerMouseUp() {
	if (!isResizing) return
	isResizing = false
	document.body.classList.remove('is-resizing-rail')
	window.removeEventListener('mousemove', onResizerMouseMove)
	window.removeEventListener('mouseup', onResizerMouseUp)
}

onMounted(() => {
	refreshAccount()
	window.addEventListener('click', closeContextMenu)
})
onUnmounted(() => {
	window.removeEventListener('click', closeContextMenu)
	window.removeEventListener('mousemove', onResizerMouseMove)
	window.removeEventListener('mouseup', onResizerMouseUp)
})
</script>

<template>
	<aside class="rail" :class="{ collapsed: isSidebarCollapsed }">
		<!-- Drag Resizer Handle on right border -->
		<div class="rail-resizer" @mousedown="onResizerMouseDown" />

		<!-- TOP HEADER: App Logo + Collapse Toggle -->
		<div class="rail-header-row">
			<div
				class="rail-brand-group"
				role="button"
				tabindex="0"
				:title="isSidebarCollapsed ? 'Desplegar barra lateral' : 'Dr3sh App — Inicio'"
				@click="isSidebarCollapsed ? toggleSidebarCollapse() : (deselectModpack(), navigateTo('home'))"
			>
				<div class="brand-logo-badge">
					<img :src="DR3SH_LOGO_DATA_URI" alt="Dr3sh" class="brand-logo-img" />
					<div v-if="isSidebarCollapsed" class="brand-expand-hover-arrow">
						<svg viewBox="0 0 20 20" aria-hidden="true" class="w-3.5 h-3.5 text-emerald-400">
							<path d="M7.5 5l5 5-5 5" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" />
						</svg>
					</div>
				</div>
				<div class="brand-text-col">
					<span class="brand-title">DR3SH</span>
					<span class="brand-sub">APP</span>
				</div>
			</div>

			<!-- Collapse Toggle Button (Arrow) -->
			<button
				type="button"
				class="rail-collapse-toggle-btn"
				:title="isSidebarCollapsed ? 'Desplegar barra lateral' : 'Plegar barra lateral'"
				@click="toggleSidebarCollapse"
			>
				<svg viewBox="0 0 20 20" aria-hidden="true" class="collapse-icon" :class="{ 'is-flipped': isSidebarCollapsed }">
					<path d="M12.5 15 7.5 10l5-5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
				</svg>
			</button>
		</div>

		<!-- 3 VERTICAL NAVIGATION ITEMS (Biblioteca, Explorar, Opciones) -->
		<div class="rail-main-nav">
			<!-- 1. Mi Biblioteca -->
			<div class="has-tooltip nav-tooltip-wrap">
				<button
					type="button"
					class="rail-nav-card"
					:class="{ active: currentView === 'home' && !selectedId }"
					@click="deselectModpack(); navigateTo('home')"
				>
					<span class="nav-card-icon">
						<svg viewBox="0 0 20 20" aria-hidden="true" class="w-5 h-5">
							<path d="M3 8.5 10 3l7 5.5v7.5a1 1 0 0 1-1 1h-4v-5H8v5H4a1 1 0 0 1-1-1V8.5Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
						</svg>
					</span>
					<span class="nav-card-label">Mi Biblioteca</span>
					<span class="nav-card-badge">{{ ownedServers.length }}</span>
				</button>
				<div class="tooltip-box">
					Mi Biblioteca ({{ ownedServers.length }})
				</div>
			</div>

			<!-- 2. Explorar -->
			<div class="has-tooltip nav-tooltip-wrap">
				<button
					type="button"
					class="rail-nav-card"
					:class="{ active: currentView === 'market' }"
					@click="navigateTo('market')"
				>
					<span class="nav-card-icon">
						<svg viewBox="0 0 20 20" aria-hidden="true" class="w-5 h-5">
							<circle cx="10" cy="10" r="7" fill="none" stroke="currentColor" stroke-width="1.8" />
							<path d="m12.8 7.2-1.8 4.6-4.6 1.8 1.8-4.6 4.6-1.8Z" fill="currentColor" stroke="none" />
						</svg>
					</span>
					<span class="nav-card-label">Explorar</span>
				</button>
				<div class="tooltip-box">
					Explorar Servidores
				</div>
			</div>

			<!-- 3. Opciones -->
			<div class="has-tooltip nav-tooltip-wrap">
				<button
					type="button"
					class="rail-nav-card"
					:class="{ active: currentView === 'settings' }"
					@click="onSettingsClick"
				>
					<span class="nav-card-icon">
						<svg viewBox="0 0 24 24" aria-hidden="true" class="w-5 h-5">
							<path
								d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"
								fill="none"
								stroke="currentColor"
								stroke-width="1.8"
							/>
							<circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" stroke-width="1.8" />
						</svg>
					</span>
					<span class="nav-card-label">Opciones</span>
				</button>
				<div class="tooltip-box">
					Opciones del Launcher
				</div>
			</div>
		</div>

		<!-- SECTION LABEL: MIS MODPACKS -->
		<div class="rail-section-head">
			<span class="section-head-label">MIS MODPACKS</span>
			<span class="rail-count">{{ ownedServers.length }}</span>
			<div class="rail-collapsed-divider" />
		</div>

		<!-- MODPACK INSTANCES LIST -->
		<nav class="server-list" aria-label="Biblioteca de modpacks">
			<div
				v-for="server in ownedServers"
				:key="server.id"
				class="has-tooltip modpack-item-wrap"
			>
				<button
					type="button"
					class="server-nav"
					:class="{
						active: selectedId === server.id && currentView === 'home',
						installed: isServerInstalled(server),
						'has-update': hasServerUpdate(server),
					}"
					@click="selectServer(server.id)"
					@contextmenu.prevent="openSidebarContextMenu($event, server)"
				>
					<span class="server-thumb">
						<img
							v-if="server.logo_square || server.logo || server.image"
							:src="resolveImageUrl(server.logo_square || server.logo || server.image)"
							:alt="server.title"
							@error="handleImageError($event)"
						/>
						<span v-else class="server-thumb-fallback">
							{{ server.title.slice(0, 2) }}
						</span>

						<!-- Status Indicator Pip in corner (1:1 + expanded) -->
						<span
							class="server-status-pip"
							:class="{
								'is-update': hasServerUpdate(server),
								'is-installed': isServerInstalled(server) && !hasServerUpdate(server),
								'is-uninstalled': !isServerInstalled(server),
							}"
						/>

						<!-- Overlay Play Button: visible on hover in 1:1 mode ONLY IF INSTALLED -->
						<span
							v-if="isServerInstalled(server)"
							class="overlay-play-btn"
							role="button"
							tabindex="0"
							:title="`Jugar a ${server.title}`"
							@click.stop="handleQuickPlay($event, server)"
						>
							<svg viewBox="0 0 20 20" aria-hidden="true" class="w-3.5 h-3.5 fill-current">
								<path d="M6 4.5 15.5 10 6 15.5V4.5Z" />
							</svg>
						</span>
					</span>

					<span class="server-nav-copy">
						<span class="server-nav-title">{{ server.title }}</span>
						<span class="server-nav-sub" :title="formatSubtitle(server)">
							<img :src="loaderIconUrl(server.mod_loader)" alt="" class="loader-logo-img" />
							<span>{{ formatSubtitle(server) }}</span>
						</span>
					</span>

					<!-- Quick Play Button on the right (expanded mode) ONLY IF INSTALLED -->
					<span
						v-if="isServerInstalled(server)"
						class="rail-quick-play-btn"
						role="button"
						tabindex="0"
						:title="`Jugar a ${server.title}`"
						@click.stop="handleQuickPlay($event, server)"
					>
						<svg viewBox="0 0 20 20" aria-hidden="true" class="w-3.5 h-3.5 fill-current">
							<path d="M6 4.5 15.5 10 6 15.5V4.5Z" />
						</svg>
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

				<!-- Floating tooltip in 1:1 mode -->
				<div class="tooltip-box">
					<div class="tooltip-title">{{ server.title }}</div>
					<div class="tooltip-sub">
						<span v-if="hasServerUpdate(server)" class="text-amber-400 font-bold">● Actualización disponible</span>
						<span v-else-if="isServerInstalled(server)" class="text-emerald-400 font-semibold">● Instalado y al día</span>
						<span v-else class="text-slate-400">● Sin instalar</span>
					</div>
					<div class="tooltip-detail">{{ formatSubtitle(server) }}</div>
				</div>
			</div>

			<div v-if="ownedServers.length === 0" class="empty-library">
				<p class="empty-library-text">No tienes modpacks en tu biblioteca.</p>
				<button type="button" class="empty-library-btn" :title="'Ir a Explorar'" @click="navigateTo('market')">
					{{ isSidebarCollapsed ? '+' : 'Ir a Explorar' }}
				</button>
			</div>
		</nav>

		<!-- BOTTOM SECTION: Profile / Account -->
		<div class="rail-bottom">
			<div class="has-tooltip w-full flex justify-center">
				<button
					type="button"
					class="profile-line"
					:class="{ 'is-collapsed': isSidebarCollapsed }"
					title="Gestionar cuentas de Minecraft"
					@click="onProfileClick"
				>
					<span class="profile-avatar">
						<img :src="userAvatarUrl" alt="" class="skin-face-img" @error="handleAvatarError" />
					</span>
					<span class="profile-copy">
						<b>{{ activeAccount?.name || 'Player' }}</b>
						<small>{{ activeAccount?.isMicrosoft ? 'Microsoft' : 'Sin conexión' }}</small>
					</span>
				</button>
				<div class="tooltip-box">
					{{ activeAccount?.name || 'Player' }} ({{ activeAccount?.isMicrosoft ? 'Microsoft' : 'Sin conexión' }})
				</div>
			</div>
		</div>

		<!-- Context Menu Teleport -->
		<Teleport to="body">
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
							@error="handleImageError($event)"
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
