<template>
	<Transition name="splash-fade" @after-leave="onAfterLeave">
		<div v-if="!doneLoading" class="splash-screen visible" data-tauri-drag-region>
			<div class="splash-logo-wrap" data-tauri-drag-region>
				<img src="/logo.png" alt="Dr3sh" class="splash-logo" data-tauri-drag-region />
				<div class="splash-spinner-wrap">
					<svg class="splash-spinner-svg" viewBox="0 0 44 44" aria-hidden="true">
						<circle class="spinner-track" cx="22" cy="22" r="18" fill="none" stroke-width="3" />
						<circle class="spinner-head" cx="22" cy="22" r="18" fill="none" stroke-width="3" stroke-linecap="round" stroke-dasharray="32 80" />
					</svg>
				</div>
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
		radial-gradient(circle at 50% 40%, rgba(27, 217, 106, 0.22), transparent 55%),
		radial-gradient(circle at 80% 20%, rgba(27, 217, 106, 0.12), transparent 45%),
		#07090e;
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
	filter: drop-shadow(0 14px 32px rgba(0, 0, 0, 0.55)) drop-shadow(0 0 20px rgba(27, 217, 106, 0.25));
}

.splash-spinner-wrap {
	display: flex;
	align-items: center;
	justify-content: center;
	width: 38px;
	height: 38px;
	background: transparent;
	overflow: visible;
}

.splash-spinner-svg {
	width: 38px;
	height: 38px;
	background: transparent;
	overflow: visible;
	animation: spin 1s linear infinite;
	transform-origin: center center;
}

.spinner-track {
	stroke: rgba(255, 255, 255, 0.15);
}

.spinner-head {
	stroke: #ffffff;
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
