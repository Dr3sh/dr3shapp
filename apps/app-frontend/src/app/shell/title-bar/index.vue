<script setup lang="ts">
import { ChevronLeftIcon, ChevronRightIcon, RightArrowIcon } from '@modrinth/assets'
import { defineMessages, IconButton, TextLogo, useVIntl } from '@modrinth/ui'
import { useRouter } from 'vue-router'

import AppActionBar from './action-bar.vue'
import Breadcrumbs from './breadcrumbs.vue'
import WindowControls from './window-controls.vue'

defineProps<{
	canNavigateBack: boolean
	canNavigateForward: boolean
	showSidebarToggle: boolean
	sidebarToggled: boolean
}>()

const emit = defineEmits<{
	'toggle-sidebar': []
}>()

const router = useRouter()
const { formatMessage } = useVIntl()
const messages = defineMessages({
	goBack: { id: 'app.navigation.go-back', defaultMessage: 'Go back' },
	goForward: { id: 'app.navigation.go-forward', defaultMessage: 'Go forward' },
	nextImage: { id: 'app.navigation.next-image', defaultMessage: 'Next image' },
})
</script>

<template>
	<div
		data-tauri-drag-region
		class="app-title-bar relative z-[2] flex bg-bg-raised pl-1 [grid-area:status] [.mac-traffic-lights_&]:pl-20"
	>
		<div data-tauri-drag-region class="flex min-w-0 flex-1 items-center overflow-hidden p-2">
			<div class="flex items-center gap-2 shrink-0 select-none pointer-events-none px-2">
				<img src="/logo.png" alt="Dr3sh" class="h-6 w-auto object-contain" />
				<span class="font-extrabold text-sm tracking-wide text-white uppercase">
					Dr3sh <span class="text-[#1bd96a]">Launcher</span>
				</span>
				<span class="px-1.5 py-0.5 text-[10px] font-bold rounded-full bg-[#1bd96a]/20 text-[#1bd96a] border border-[#1bd96a]/30">v1.2.0</span>
			</div>
			<div data-tauri-drag-region class="ml-2 flex shrink-0 items-center gap-2">
				<IconButton
					type="outlined"
					:label="formatMessage(messages.goBack)"
					class="!h-7 !min-w-7 !w-7 !border !border-surface-4 !p-0 !opacity-100"
					:disabled="!canNavigateBack"
					@click="router.back()"
				>
					<ChevronLeftIcon
						class="!size-4 !text-primary"
						:class="{ 'opacity-20': !canNavigateBack }"
					/>
				</IconButton>
				<IconButton
					type="outlined"
					:label="formatMessage(messages.goForward)"
					class="!h-7 !min-w-7 !w-7 !border !border-surface-4 !p-0 !opacity-100"
					:disabled="!canNavigateForward"
					@click="router.forward()"
				>
					<ChevronRightIcon
						class="!size-4 !text-primary"
						:class="{ 'opacity-20': !canNavigateForward }"
					/>
				</IconButton>
			</div>
			<Breadcrumbs />
		</div>
		<section data-tauri-drag-region class="flex shrink-0 ml-auto items-center">
			<IconButton
				v-if="showSidebarToggle"
				:type="sidebarToggled ? 'base' : 'quiet'"
				:label="formatMessage(messages.nextImage)"
				class="mr-3 transition-transform"
				:class="{ 'rotate-180': !sidebarToggled }"
				@click="emit('toggle-sidebar')"
			>
				<RightArrowIcon />
			</IconButton>
			<div class="flex mr-3">
				<Suspense>
					<AppActionBar />
				</Suspense>
			</div>
			<WindowControls />
		</section>
	</div>
</template>

<style scoped>
.app-title-bar {
	height: var(--top-bar-height);
	padding-right: var(--window-controls-width, 0px);
}
</style>
