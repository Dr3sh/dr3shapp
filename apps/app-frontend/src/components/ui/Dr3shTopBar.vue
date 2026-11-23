<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useDr3shLauncher } from '@/composables/use-dr3sh-launcher'

const router = useRouter()
const {
	launcherVersion,
	activeAccount,
	currentView,
	activeSettingsTab,
	isOffline,
	catalogError,
	retryingCatalog,
	fetchVersionsData,
	handleAvatarError,
} = useDr3shLauncher()

function onProfileClick() {
	activeSettingsTab.value = 'account'
	currentView.value = 'settings'
	if (router.currentRoute.value.path !== '/') {
		router.push('/')
	}
}

const isMaximized = ref(false)

const userAvatarUrl = computed(() => {
	if (activeAccount.value?.uuid && activeAccount.value?.isMicrosoft) {
		return `https://mc-heads.net/avatar/${activeAccount.value.uuid}/32`
	}
	if (activeAccount.value?.name) {
		return `https://mc-heads.net/avatar/${encodeURIComponent(activeAccount.value.name)}/32`
	}
	return 'https://mc-heads.net/avatar/Player/32'
})

onMounted(async () => {
	try {
		const win = getCurrentWindow()
		isMaximized.value = await win.isMaximized()
		win.onResized(async () => {
			isMaximized.value = await win.isMaximized()
		})
	} catch {}
})

async function minimize() {
	try {
		await getCurrentWindow().minimize()
	} catch {}
}

async function toggleMaximize() {
	try {
		await getCurrentWindow().toggleMaximize()
		isMaximized.value = await getCurrentWindow().isMaximized()
	} catch {}
}

async function close() {
	try {
		await getCurrentWindow().close()
	} catch {}
}
</script>

<template>
	<header data-tauri-drag-region class="topbar">
		<div class="topbar-brand" data-tauri-drag-region>
			<span class="topbar-app-ver">v{{ launcherVersion }}</span>
		</div>

		<div class="topbar-spacer" data-tauri-drag-region />

		<div class="top-meta" data-tauri-drag-region-exclude>
			<button
				v-if="isOffline || catalogError"
				type="button"
				class="topbar-offline-pill"
				:disabled="retryingCatalog"
				title="Haz clic para reintentar conexión"
				@click="fetchVersionsData(true)"
			>
				<svg
					v-if="retryingCatalog"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2.2"
					stroke-linecap="round"
					stroke-linejoin="round"
					width="13"
					height="13"
					class="topbar-offline-icon is-spinning"
				>
					<path d="M21 12a9 9 0 1 1-6.219-8.56" />
				</svg>
				<svg
					v-else
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2.2"
					stroke-linecap="round"
					stroke-linejoin="round"
					width="13"
					height="13"
					class="topbar-offline-icon"
				>
					<path d="M12 20h.01" />
					<path d="M8.5 16.429a5 5 0 0 1 7 0" />
					<path d="M5 12.859a10 10 0 0 1 5.17-2.69" />
					<path d="M19 12.859a10 10 0 0 0-2.007-1.523" />
					<path d="M2 8.82a15 15 0 0 1 4.177-2.643" />
					<path d="M22 8.82a15 15 0 0 0-11.288-3.764" />
					<path d="m2 2 20 20" />
				</svg>
				<span>{{ retryingCatalog ? 'Reconectando...' : isOffline ? 'Sin conexión' : 'Catálogo sin conexión' }}</span>
			</button>

			<!-- User Profile Pill -->
			<div
				class="topbar-user-badge"
				role="button"
				tabindex="0"
				:title="`Sesión: ${activeAccount?.name || 'Player'} (Gestionar cuentas)`"
				style="cursor: pointer"
				@click="onProfileClick"
			>
				<img :src="userAvatarUrl" alt="" class="topbar-user-avatar" @error="handleAvatarError" />
				<span class="topbar-user-name">{{ activeAccount?.name || 'Player' }}</span>
			</div>
		</div>

		<div class="window-controls" aria-label="Controles de ventana" data-tauri-drag-region-exclude>
			<button class="window-control" title="Minimizar" aria-label="Minimizar" @click="minimize">
				<span class="minimize-glyph" />
			</button>
			<button
				class="window-control"
				:title="isMaximized ? 'Restaurar' : 'Maximizar'"
				aria-label="Maximizar"
				@click="toggleMaximize"
			>
				<span :class="isMaximized ? 'restore-glyph' : 'maximize-glyph'" />
			</button>
			<button class="window-control close-control" title="Cerrar" aria-label="Cerrar" @click="close">
				<span class="close-glyph" />
			</button>
		</div>
	</header>
</template>
