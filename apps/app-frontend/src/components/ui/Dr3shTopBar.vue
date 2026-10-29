<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useDr3shLauncher } from '@/composables/use-dr3sh-launcher'

const {
	selectedServer,
	currentView,
	loading,
	maintenanceMode,
	testingMode,
	testingHours,
	fetchVersionsData,
} = useDr3shLauncher()

const isMaximized = ref(false)

const title = computed(() => {
	if (currentView.value === 'market') return 'Explorar modpacks'
	if (currentView.value === 'settings') return 'Ajustes'
	return selectedServer.value?.title || 'Sin instancias'
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
		<div class="crumb" data-tauri-drag-region>
			<b>{{ title }}</b>
		</div>

		<div class="top-meta" data-tauri-drag-region-exclude>
			<div
				v-if="maintenanceMode"
				class="admin-status-pill admin-pill-maintenance"
				title="Modo Mantenimiento activo: el launcher está bloqueado para los jugadores ordinarios"
			>
				<span class="admin-pill-dot" />
				<span>Mantenimiento Activo (Admin)</span>
			</div>
			<div
				v-if="testingMode"
				class="admin-status-pill admin-pill-testing"
				:title="`Modo Testing activo: las instancias de los jugadores se eliminarán tras ${testingHours}h cerrado`"
			>
				<span class="admin-pill-dot" />
				<span>Modo Testing Activo</span>
			</div>
			<span class="topbar-version-pill" title="Versión del launcher: v0.2.16">v0.2.16</span>
			<button
				type="button"
				class="icon-button"
				:class="{ spin: loading }"
				title="Refrescar catálogo"
				aria-label="Refrescar catálogo"
				:disabled="loading"
				@click="fetchVersionsData()"
			>
				<svg viewBox="0 0 20 20" aria-hidden="true">
					<path d="M16.5 10a6.5 6.5 0 1 1-1.9-4.6M16.5 4v3.8h-3.8" />
				</svg>
			</button>
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
