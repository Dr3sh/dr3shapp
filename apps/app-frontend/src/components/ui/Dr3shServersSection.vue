<script setup lang="ts">
import { computed, inject, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

interface Screenshot {
	url: string
	status?: string
}

interface ServerVersion {
	id: string
	server: string
	title: string
	version?: string
	description?: string
	link?: string
	manifest?: string
	minecraft_version?: string
	mod_loader?: string
	mod_loader_version?: string
	size_mb?: number
	published_at?: string
	status?: string
}

interface Dr3shServer {
	id: string
	title: string
	status: string
	image?: string
	logo?: string
	logo_square?: string
	theme_color?: string
	description?: string
	minecraft_version?: string
	mod_loader?: string
	mod_loader_version?: string
	screenshots?: Screenshot[]
	server_address?: string
}

const props = defineProps<{
	instances: any[]
}>()

const showCreationModal = inject<() => void>('showCreationModal')

const servers = ref<Dr3shServer[]>([])
const versions = ref<ServerVersion[]>([])
const selectedServerId = ref<string>('opus')
const activeScreenshotIndex = ref(0)
const loading = ref(true)
const copiedIp = ref(false)

function resolveImageUrl(img?: string) {
	if (!img) return ''
	if (img.startsWith('http://') || img.startsWith('https://')) return img
	return `https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/${img.replace(/^\/+/, '')}`
}

async function loadData() {
	try {
		loading.value = true
		const res = await fetch('https://raw.githubusercontent.com/Dr3sh/Dr3sh.github.io/main/versions.json')
		if (res.ok) {
			const data = await res.json()
			servers.value = (data.servers || []).filter((s: Dr3shServer) => s.status !== 'hidden')
			versions.value = data.versions || []
			if (servers.value.length > 0 && !servers.value.some(s => s.id === selectedServerId.value)) {
				selectedServerId.value = servers.value[0].id
			}
		}
	} catch (e) {
		console.error('Error fetching Dr3sh versions:', e)
	} finally {
		loading.value = false
	}
}

onMounted(() => {
	loadData()
})

const currentServer = computed(() => {
	return servers.value.find(s => s.id === selectedServerId.value) || servers.value[0]
})

const currentVersion = computed(() => {
	if (!currentServer.value) return null
	return versions.value.find(v => v.server === currentServer.value.id) || null
})

const screenshots = computed(() => {
	if (!currentServer.value?.screenshots) return []
	return currentServer.value.screenshots.filter(s => s.status !== 'hidden')
})

const installedInstance = computed(() => {
	if (!currentServer.value) return null
	const id = currentServer.value.id.toLowerCase()
	const title = currentServer.value.title.toLowerCase()
	return props.instances?.find((inst: any) => {
		const name = (inst.name || '').toLowerCase()
		return name.includes(id) || name.includes(title)
	})
})

const isInstalled = computed(() => Boolean(installedInstance.value))

function copyServerAddress() {
	const address = currentServer.value?.server_address || 'play.dr3sh.net'
	navigator.clipboard.writeText(address)
	copiedIp.value = true
	setTimeout(() => {
		copiedIp.value = false
	}, 2000)
}

async function handlePrimaryAction() {
	if (isInstalled.value && installedInstance.value) {
		try {
			await invoke('plugin:process|launch', { instanceId: installedInstance.value.id })
		} catch (err) {
			console.error('Error launching instance:', err)
		}
	} else {
		showCreationModal?.()
	}
}
</script>

<template>
	<div class="dr3sh-view flex flex-col gap-6">
		<!-- Server Navigation Tabs -->
		<div class="server-tabs-bar flex items-center justify-between gap-3 flex-wrap">
			<div class="flex items-center gap-2 overflow-x-auto pb-1">
				<button
					v-for="server in servers"
					:key="server.id"
					type="button"
					class="server-tab-btn flex items-center gap-2.5 px-4 py-2 rounded-xl text-sm font-semibold transition-all duration-200 border"
					:class="
						selectedServerId === server.id
							? 'bg-surface-3 text-white border-brand shadow-[0_0_15px_rgba(27,217,106,0.18)]'
							: 'bg-surface-2 text-secondary border-surface-4 hover:border-surface-5 hover:text-primary'
					"
					@click="
						selectedServerId = server.id;
						activeScreenshotIndex = 0;
					"
				>
					<img
						v-if="server.logo_square || server.logo || server.image"
						:src="resolveImageUrl(server.logo_square || server.logo || server.image)"
						:alt="server.title"
						class="w-5 h-5 rounded-md object-cover"
					/>
					<span class="truncate">{{ server.title }}</span>
					<span
						v-if="server.status === 'active'"
						class="w-2 h-2 rounded-full bg-[#1bd96a]"
						:class="{ 'shadow-[0_0_8px_#1bd96a]': selectedServerId === server.id }"
					></span>
				</button>
			</div>

			<div class="flex items-center gap-2">
				<button
					type="button"
					class="px-3 py-1.5 rounded-lg text-xs font-semibold bg-surface-2 hover:bg-surface-3 text-secondary hover:text-white border border-surface-4 transition-colors flex items-center gap-1.5"
					@click="loadData"
				>
					<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
						<path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67" />
					</svg>
					Actualizar
				</button>
			</div>
		</div>

		<!-- HERO BANNER (Estilo Dr3sh Launcher / Modrinth Hero) -->
		<section v-if="currentServer" class="modrinth-hero relative rounded-[22px] overflow-hidden border border-surface-4/70 bg-[#151923] shadow-2xl transition-all">
			<!-- Background Image Backdrop with Blur -->
			<div
				class="modrinth-hero-backdrop absolute inset-0 bg-cover bg-center pointer-events-none opacity-25 saturate-150 blur-xl"
				:style="{
					backgroundImage: `url('${resolveImageUrl(
						screenshots[activeScreenshotIndex]?.url || currentServer.image || currentServer.logo
					)}')`,
				}"
			></div>
			<div class="absolute inset-0 bg-gradient-to-r from-[#0d1017] via-[#0d1017]/85 to-transparent pointer-events-none"></div>

			<!-- Hero Content -->
			<div class="relative z-10 flex flex-col md:flex-row items-start md:items-center justify-between gap-6 p-6 md:p-8">
				<div class="flex items-start md:items-center gap-5 min-w-0 flex-1">
					<!-- Pack Avatar Icon with Border -->
					<div
						class="w-20 h-20 md:w-24 md:h-24 rounded-2xl overflow-hidden shrink-0 bg-[#090b10] border border-white/15 shadow-xl"
						:style="{
							boxShadow: currentServer.theme_color
								? `0 10px 28px ${currentServer.theme_color}55`
								: '0 10px 28px rgba(0,0,0,0.45)',
						}"
					>
						<img
							v-if="currentServer.logo_square || currentServer.logo || currentServer.image"
							:src="resolveImageUrl(currentServer.logo_square || currentServer.logo || currentServer.image)"
							:alt="currentServer.title"
							class="w-full h-full object-cover"
						/>
						<div v-else class="w-full h-full flex items-center justify-center font-bold text-2xl text-secondary">
							{{ currentServer.title.slice(0, 2) }}
						</div>
					</div>

					<!-- Meta Information -->
					<div class="flex flex-col gap-2 min-w-0">
						<div class="flex items-center gap-3 flex-wrap">
							<h1 class="text-2xl md:text-3xl font-extrabold text-white tracking-tight m-0">
								{{ currentServer.title }}
							</h1>
							<span
								class="px-2.5 py-0.5 text-xs font-bold rounded-full uppercase tracking-wider flex items-center gap-1.5"
								:class="currentServer.status === 'active' ? 'bg-[#1bd96a]/20 text-[#1bd96a] border border-[#1bd96a]/40' : 'bg-surface-4 text-secondary'"
							>
								<span v-if="currentServer.status === 'active'" class="w-1.5 h-1.5 rounded-full bg-[#1bd96a] animate-pulse"></span>
								{{ currentServer.status === 'active' ? 'Oficial • Activo' : 'WIP' }}
							</span>
						</div>

						<p class="text-sm text-[#cbd5e1] leading-relaxed max-w-2xl m-0 line-clamp-2">
							{{ currentServer.description || 'Servidor y modpack oficial de la comunidad de Dr3sh.' }}
						</p>

						<!-- Badges Row -->
						<div class="flex items-center gap-2 flex-wrap mt-1">
							<span
								v-if="currentServer.mod_loader"
								class="meta-badge px-2.5 py-1 text-xs font-semibold rounded-lg bg-surface-3/80 border border-surface-5/80 text-white flex items-center gap-1.5 uppercase tracking-wide"
							>
								<span class="w-2 h-2 rounded-full bg-emerald-400"></span>
								{{ currentServer.mod_loader }}
								<small v-if="currentVersion?.mod_loader_version" class="text-secondary font-mono">{{ currentVersion.mod_loader_version }}</small>
							</span>

							<span
								v-if="currentServer.minecraft_version"
								class="meta-badge px-2.5 py-1 text-xs font-semibold rounded-lg bg-surface-3/80 border border-surface-5/80 text-white flex items-center gap-1.5"
							>
								Minecraft {{ currentServer.minecraft_version }}
							</span>

							<span
								v-if="currentVersion?.version || currentVersion?.title"
								class="meta-badge px-2.5 py-1 text-xs font-semibold rounded-lg bg-surface-3/80 border border-surface-5/80 text-secondary"
							>
								{{ currentVersion.version ? `v${currentVersion.version}` : currentVersion.title }}
							</span>

							<span
								v-if="currentVersion?.size_mb"
								class="meta-badge px-2.5 py-1 text-xs font-semibold rounded-lg bg-surface-3/80 border border-surface-5/80 text-secondary"
							>
								{{ currentVersion.size_mb >= 1024 ? `${(currentVersion.size_mb / 1024).toFixed(1)} GB` : `${currentVersion.size_mb} MB` }}
							</span>
						</div>
					</div>
				</div>

				<!-- Play / Install Actions -->
				<div class="flex flex-col sm:flex-row md:flex-col items-stretch md:items-end gap-3 w-full md:w-auto shrink-0">
					<button
						type="button"
						class="play-button h-12 px-7 rounded-xl font-extrabold text-sm tracking-wide text-black flex items-center justify-center gap-2.5 transition-all duration-200 cursor-pointer select-none active:scale-95"
						:class="
							isInstalled
								? 'bg-[#1bd96a] hover:bg-[#16bd5a] shadow-[0_8px_24px_rgba(27,217,106,0.4)]'
								: 'bg-emerald-400 hover:bg-emerald-300 shadow-[0_8px_24px_rgba(52,211,153,0.35)]'
						"
						@click="handlePrimaryAction"
					>
						<svg v-if="isInstalled" viewBox="0 0 20 20" width="16" height="16" fill="currentColor">
							<path d="M6 4.5 15.5 10 6 15.5V4.5Z" />
						</svg>
						<svg v-else viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
							<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3" />
						</svg>
						<span>{{ isInstalled ? 'JUGAR' : 'INSTALAR' }}</span>
					</button>

					<div class="flex items-center justify-end gap-2 text-xs">
						<button
							type="button"
							class="px-3 py-1.5 rounded-lg bg-surface-3 hover:bg-surface-4 text-secondary hover:text-white border border-surface-5 transition-colors flex items-center gap-1.5"
							@click="copyServerAddress"
						>
							<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
								<path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
							</svg>
							<span>{{ copiedIp ? '¡IP Copiada!' : 'Copiar IP' }}</span>
						</button>
					</div>
				</div>
			</div>
		</section>

		<!-- SCREENSHOTS GALLERY -->
		<section v-if="screenshots.length > 0" class="flex flex-col gap-3">
			<div class="flex items-center justify-between">
				<h3 class="text-base font-bold text-white flex items-center gap-2 m-0">
					<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="text-brand">
						<rect x="3" y="3" width="18" height="18" rx="2" />
						<circle cx="8.5" cy="8.5" r="1.5" />
						<polyline points="21 15 16 10 5 21" />
					</svg>
					Capturas del Servidor
				</h3>
				<span class="text-xs text-secondary">{{ activeScreenshotIndex + 1 }} de {{ screenshots.length }}</span>
			</div>

			<!-- Main Preview -->
			<div class="relative h-64 md:h-80 w-full rounded-2xl overflow-hidden border border-surface-4 bg-surface-1 shadow-lg group">
				<img
					:src="resolveImageUrl(screenshots[activeScreenshotIndex]?.url)"
					:alt="`Screenshot ${activeScreenshotIndex + 1}`"
					class="w-full h-full object-cover transition-transform duration-700 group-hover:scale-102"
				/>
				<div class="absolute inset-0 bg-gradient-to-t from-black/60 via-transparent to-transparent pointer-events-none"></div>

				<!-- Navigation arrows -->
				<button
					v-if="screenshots.length > 1"
					type="button"
					class="absolute left-3 top-1/2 -translate-y-1/2 w-9 h-9 rounded-full bg-black/60 hover:bg-black/80 text-white flex items-center justify-center backdrop-blur-md transition-all active:scale-90"
					@click="activeScreenshotIndex = (activeScreenshotIndex - 1 + screenshots.length) % screenshots.length"
				>
					<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
						<polyline points="15 18 9 12 15 6" />
					</svg>
				</button>
				<button
					v-if="screenshots.length > 1"
					type="button"
					class="absolute right-3 top-1/2 -translate-y-1/2 w-9 h-9 rounded-full bg-black/60 hover:bg-black/80 text-white flex items-center justify-center backdrop-blur-md transition-all active:scale-90"
					@click="activeScreenshotIndex = (activeScreenshotIndex + 1) % screenshots.length"
				>
					<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
						<polyline points="9 18 15 12 9 6" />
					</svg>
				</button>
			</div>

			<!-- Thumbnails row -->
			<div v-if="screenshots.length > 1" class="flex items-center gap-2.5 overflow-x-auto pb-1">
				<button
					v-for="(img, idx) in screenshots"
					:key="idx"
					type="button"
					class="h-14 w-24 shrink-0 rounded-xl overflow-hidden border-2 transition-all cursor-pointer relative"
					:class="
						activeScreenshotIndex === idx
							? 'border-brand shadow-[0_0_10px_rgba(27,217,106,0.3)] opacity-100 scale-102'
							: 'border-surface-4 opacity-50 hover:opacity-90'
					"
					@click="activeScreenshotIndex = idx"
				>
					<img :src="resolveImageUrl(img.url)" :alt="`Thumb ${idx + 1}`" class="w-full h-full object-cover" />
				</button>
			</div>
		</section>

		<!-- CHANGELOG & DETAILS CARD -->
		<section v-if="currentVersion" class="rounded-2xl border border-surface-4 bg-surface-2 p-5 flex flex-col gap-3">
			<div class="flex items-center justify-between">
				<h4 class="text-sm font-bold text-white uppercase tracking-wider flex items-center gap-2 m-0">
					<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="text-brand">
						<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
						<polyline points="14 2 14 8 20 8" />
						<line x1="16" y1="13" x2="8" y2="13" />
						<line x1="16" y1="17" x2="8" y2="17" />
					</svg>
					Notas de versión • {{ currentVersion.title }}
				</h4>
				<span v-if="currentVersion.published_at" class="text-xs text-secondary font-mono">{{ currentVersion.published_at }}</span>
			</div>
			<p class="text-xs text-secondary leading-relaxed m-0 whitespace-pre-wrap">
				{{ currentVersion.description || 'Sin notas adicionales para esta versión.' }}
			</p>
		</section>
	</div>
</template>

<style scoped>
.play-button {
	font-family: inherit;
}
.meta-badge {
	backdrop-filter: blur(8px);
}
</style>
