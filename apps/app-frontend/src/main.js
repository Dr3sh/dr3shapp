import 'overlayscrollbars/overlayscrollbars.css'

import { installTooltipDirective } from '@modrinth/ui'
import { VueQueryPlugin } from '@tanstack/vue-query'
import { createApp } from 'vue'

import App from '@/App.vue'
import { overlayScrollbarsDirective } from '@/directives/overlayScrollbars'
import { setupErrorReporting } from '@/helpers/error-reporting'
import { debugStartup, traceStartupStep } from '@/helpers/startup-debug'
import i18nPlugin from '@/plugins/i18n'
import i18nDebugPlugin from '@/plugins/i18n-debug'
import router from '@/routes'

debugStartup('Frontend entry module evaluated')
const app = createApp(App)
setupErrorReporting(app, router)

app.use(VueQueryPlugin)
app.use(router)
if (import.meta.env.DEV) {
	void traceStartupStep('Initial router readiness', () => router.isReady()).catch(() => {})
}
app.use(i18nPlugin)
app.use(i18nDebugPlugin)
installTooltipDirective(app)
app.directive('overlay-scrollbars', overlayScrollbarsDirective)

async function mount() {
	if (import.meta.env.DEV && import.meta.env.VITE_VUE_SCAN === 'true') {
		const { VueScanPlugin } = await traceStartupStep(
			'Load Vue render tracker',
			() => import('@taijased/vue-render-tracker'),
		)
		app.use(new VueScanPlugin({ enabled: true, showOverlay: true, log: false, playSound: false }))
	}
	debugStartup('Vue mount started')
	app.mount('#app')
	debugStartup('Vue mount completed')
	if (typeof document !== 'undefined' && 'FontFace' in window && document.fonts) {
		const fontDefs = [
			['Plus Jakarta Sans', '/fonts/PlusJakartaSans-Regular.ttf', '400'],
			['Plus Jakarta Sans', '/fonts/PlusJakartaSans-Medium.ttf', '500'],
			['Plus Jakarta Sans', '/fonts/PlusJakartaSans-SemiBold.ttf', '600'],
			['Plus Jakarta Sans', '/fonts/PlusJakartaSans-Bold.ttf', '700'],
			['Plus Jakarta Sans', '/fonts/PlusJakartaSans-ExtraBold.ttf', '800'],
			['JetBrains Mono', '/fonts/JetBrainsMono-Regular.ttf', '400'],
			['JetBrains Mono', '/fonts/JetBrainsMono-SemiBold.ttf', '600'],
		]
		void Promise.allSettled(
			fontDefs.map(async ([family, url, weight]) => {
				const face = new FontFace(family, `url('${url}')`, { weight, style: 'normal' })
				await face.load()
				document.fonts.add(face)
			}),
		).then(() => {
			if (import.meta.env.DEV) {
				const loaded = [...document.fonts]
					.filter((f) => f.status === 'loaded')
					.map((f) => `${f.family}:${f.weight}`)
				console.info(`[Fonts] loaded=[${[...new Set(loaded)].join(', ')}]`)
			}
		})
	}
}

void mount()
