<template>
	<Transition name="splash-fade" @after-leave="onAfterLeave">
		<div v-if="!doneLoading" class="splash-screen visible" data-tauri-drag-region>
			<div class="splash-logo-wrap" data-tauri-drag-region>
				<img src="/logo.png" alt="Dr3sh" class="splash-logo" data-tauri-drag-region />
				<svg class="splash-spinner-svg" viewBox="0 0 40 40" aria-hidden="true">
					<circle cx="20" cy="20" r="16" />
				</svg>
				<span v-if="message" class="dr3sh-splash-message">{{ message }}</span>
			</div>
		</div>
	</Transition>
</template>

<script setup>
import { injectLoadingState } from '@modrinth/ui'
import { onMounted, ref, watch } from 'vue'

import { useAppEvent } from '@/composables/use-app-event'
import { debugStartup } from '@/helpers/startup-debug'

const doneLoading = ref(false)
const loadingProgress = ref(0)
const message = ref()

const MIN_DISPLAY_MS = 450
const mountedAt = Date.now()

const loading = injectLoadingState()
onMounted(() => debugStartup('Splash mounted'))

function onAfterLeave() {
	debugStartup('Splash fade completed', { displayedMs: Date.now() - mountedAt })
	loading.setEnabled(true)
}

watch(
	[loading.barEnabled, loading.pending],
	([barEnabled, pending]) => {
		debugStartup('Splash loading state changed', { barEnabled, pending })
		if (barEnabled) {
			return
		}

		if (pending) {
			loadingProgress.value = 0
			fakeLoadingIncrease()
			return
		}

		const elapsed = Date.now() - mountedAt
		const delay = Math.max(0, MIN_DISPLAY_MS - elapsed)
		debugStartup('Splash dismissal scheduled', { delayMs: delay, displayedMs: elapsed })

		setTimeout(() => {
			if (loading.pending.value) {
				debugStartup('Splash dismissal deferred: new loading work')
				return
			}
			doneLoading.value = true
			debugStartup('Splash fade started', { displayedMs: Date.now() - mountedAt })
		}, delay)
	},
	{ immediate: true },
)

function fakeLoadingIncrease() {
	if (loadingProgress.value < 95) {
		setTimeout(() => {
			loadingProgress.value += 2
			fakeLoadingIncrease()
		}, 5)
	}
}

useAppEvent('loading', (e) => {
	if (e.event.type === 'directory_move') {
		loadingProgress.value = 100 * (e.fraction ?? 1)
		message.value = 'Actualizando directorio del launcher...'
	}
})
</script>

<style scoped lang="scss">
.splash-screen {
	position: fixed;
	inset: 0;
	z-index: 10000;
	display: flex;
	align-items: center;
	justify-content: center;
	background:
		radial-gradient(circle at 50% 42%, color-mix(in srgb, var(--brand, #2f65f6) 16%, transparent), transparent 54%),
		#0d1017;
}

.splash-fade-leave-active {
	transition: opacity 0.28s ease-in-out;
}

.splash-fade-leave-to {
	opacity: 0;
}

.splash-logo-wrap {
	display: flex;
	flex-direction: column;
	align-items: center;
	justify-content: center;
	gap: 28px;
}

.splash-logo {
	width: 170px;
	height: auto;
	object-fit: contain;
	filter: drop-shadow(0 14px 32px rgba(0, 0, 0, 0.55));
}

.splash-spinner-svg {
	width: 38px;
	height: 38px;
	animation: spin 1.05s linear infinite;
}

.splash-spinner-svg circle {
	fill: none;
	stroke: var(--brand, #2f65f6);
	stroke-width: 3.5;
	stroke-linecap: round;
	stroke-dasharray: 64 42;
}

.dr3sh-splash-message {
	font-size: 12px;
	color: rgba(255, 255, 255, 0.65);
	letter-spacing: 0.02em;
}

@keyframes spin {
	from {
		transform: rotate(0deg);
	}
	to {
		transform: rotate(360deg);
	}
}
</style>
