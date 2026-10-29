<script setup lang="ts">
import {
	CodeIcon,
	CoffeeIcon,
	EyeOffIcon,
	GaugeIcon,
	HeartHandshakeIcon,
	LanguagesIcon,
	LightBulbIcon,
	ModrinthIcon,
	PaintbrushIcon,
	RefreshCwIcon,
	Settings2Icon,
	ToggleRightIcon,
	UserIcon,
	WindowIcon,
	WrenchIcon,
} from '@modrinth/assets'
import {
	commonMessages,
	commonSettingsMessages,
	defineMessage,
	defineMessages,
	injectNotificationManager,
	ProgressBar,
	TabbedModal,
	UnsavedChangesPopup,
	useVIntl,
} from '@modrinth/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query'
import { getVersion } from '@tauri-apps/api/app'
import { platform as getOsPlatform, version as getOsVersion } from '@tauri-apps/plugin-os'
import { computed, ref } from 'vue'

import { useAppSettings } from '@/composables/use-app-settings.ts'
import MinecraftAccountsSettings from '@/features/settings/account/minecraft-accounts-settings.vue'
import AppearanceSettings from '@/features/settings/display/appearance-settings.vue'
import LanguageSettings from '@/features/settings/display/language-settings.vue'
import JavaAndMemorySettings from '@/features/settings/instance-defaults/java-and-memory-settings/index.vue'
import WindowSettings from '@/features/settings/instance-defaults/window-settings.vue'
import type { InstanceDefaultsTab } from '@/features/settings/settings-actions'
import ResourceManagementSettings from '@/features/settings/system/resource-management-settings/index.vue'
import TroubleshootingSettings from '@/features/settings/system/troubleshooting-settings.vue'
import { appSettingsKeys, appSettingsQueryOptions, set } from '@/helpers/settings.ts'
import { injectAppUpdateDownloadProgress } from '@/providers/download-progress.ts'

import { provideAppSettingsModal, type UnsavedChangesController } from './context'

// TODO: Apply COMPONENT_STRUCTURE.md here and extract out common setting option components
const appSettings = useAppSettings()

const { formatMessage } = useVIntl()
const { handleError } = injectNotificationManager()
const queryClient = useQueryClient()

const devModeCounter = ref(0)

const developerModeEnabled = defineMessage({
	id: 'app.settings.developer-mode-enabled',
	defaultMessage: 'Developer mode enabled.',
})

const tabCategories = defineMessages({
	display: {
		id: 'settings.sidebar.label.display',
		defaultMessage: 'Display',
	},
	account: {
		id: 'settings.sidebar.label.account',
		defaultMessage: 'Account',
	},
	instanceDefaults: {
		id: 'app.settings.sidebar.label.instance-defaults',
		defaultMessage: 'Instance defaults',
	},
	system: {
		id: 'app.settings.sidebar.label.system',
		defaultMessage: 'System',
	},
})

const tabs = [
	{
		name: defineMessage({
			id: 'app.settings.tabs.accounts',
			defaultMessage: 'Cuentas de Minecraft',
		}),
		category: tabCategories.account,
		icon: UserIcon,
		content: MinecraftAccountsSettings,
	},
	{
		name: defineMessage({
			id: 'app.settings.tabs.java-and-memory',
			defaultMessage: 'Java y memoria',
		}),
		category: tabCategories.instanceDefaults,
		icon: CoffeeIcon,
		content: JavaAndMemorySettings,
	},
	{
		name: defineMessage({
			id: 'app.settings.tabs.window',
			defaultMessage: 'Ventana de juego',
		}),
		category: tabCategories.instanceDefaults,
		icon: WindowIcon,
		content: WindowSettings,
	},
	{
		name: defineMessage({
			id: 'app.settings.tabs.appearance',
			defaultMessage: 'Apariencia',
		}),
		category: tabCategories.display,
		icon: PaintbrushIcon,
		content: AppearanceSettings,
	},
	{
		name: defineMessage({
			id: 'app.settings.tabs.language',
			defaultMessage: 'Idioma',
		}),
		category: tabCategories.display,
		icon: LanguagesIcon,
		content: LanguageSettings,
		badge: commonMessages.beta,
	},
	{
		name: defineMessage({
			id: 'app.settings.tabs.resources',
			defaultMessage: 'Almacenamiento y recursos',
		}),
		category: tabCategories.system,
		icon: GaugeIcon,
		content: ResourceManagementSettings,
	},
	{
		name: defineMessage({
			id: 'app.settings.tabs.troubleshooting',
			defaultMessage: 'Diagnóstico',
		}),
		category: tabCategories.system,
		icon: WrenchIcon,
		content: TroubleshootingSettings,
	},
]

const availableTabs = computed(() =>
	tabs.filter((tab) => !tab.developerOnly || appSettings.devMode),
)

const modal = ref<InstanceType<typeof TabbedModal> | null>(null)
const unsavedChangesPopup = ref<{ nudge: () => void } | null>(null)
const unsavedChangesController = ref<UnsavedChangesController | null>(null)
const emptyUnsavedChangesState: Record<string, unknown> = {}
const originalUnsavedChangesState = computed(
	() => unsavedChangesController.value?.getOriginal() ?? emptyUnsavedChangesState,
)
const modifiedUnsavedChangesState = computed(
	() => unsavedChangesController.value?.getModified() ?? emptyUnsavedChangesState,
)
const savingUnsavedChanges = computed(() => unsavedChangesController.value?.isSaving() ?? false)
const hasUnsavedChanges = computed(
	() =>
		(unsavedChangesController.value?.hasChanges() ?? false) ||
		(unsavedChangesController.value?.isSaving() ?? false),
)

function canLeaveCurrentTab(): boolean {
	if (
		!unsavedChangesController.value?.hasChanges() &&
		!unsavedChangesController.value?.isSaving()
	) {
		return true
	}
	unsavedChangesPopup.value?.nudge()
	return false
}

function close(): boolean {
	return modal.value?.hide() ?? false
}

function registerUnsavedChangesController(controller: UnsavedChangesController | null): void {
	unsavedChangesController.value = controller
}

provideAppSettingsModal({
	close,
	registerUnsavedChangesController,
})

function resetUnsavedChanges(): void {
	unsavedChangesController.value?.reset()
}

function saveUnsavedChanges(): void {
	void unsavedChangesController.value?.save()
}

function show() {
	modal.value?.show()
}

function showAccounts(): void {
	const accountsIndex = availableTabs.value.findIndex(
		(tab) => tab.content === MinecraftAccountsSettings,
	)
	if (accountsIndex >= 0) {
		modal.value?.setTab(accountsIndex)
	}
	modal.value?.show()
}

function showProfile(): void {
	showAccounts()
}

function showFeatureFlags(): void {
	modal.value?.show()
}

function showSyncedOptions(): void {
	modal.value?.show()
}

function showInstanceDefaults(tab: InstanceDefaultsTab): void {
	const content = {
		java: JavaAndMemorySettings,
		window: WindowSettings,
		hooks: JavaAndMemorySettings,
	}[tab]
	const tabIndex = availableTabs.value.findIndex((tab) => tab.content === content)
	if (tabIndex >= 0) {
		modal.value?.setTab(tabIndex)
	}
	modal.value?.show()
}

defineExpose({ show, showAccounts, showProfile, showFeatureFlags, showSyncedOptions, showInstanceDefaults })

const { progress, version: downloadingVersion } = injectAppUpdateDownloadProgress()

const { data: appInfo } = useQuery({
	queryKey: ['app-info'],
	queryFn: async () => ({
		version: await getVersion(),
		osPlatform: getOsPlatform(),
		osVersion: getOsVersion(),
	}),
	staleTime: Infinity,
})

const developerModeMutation = useMutation({
	mutationKey: appSettingsKeys.update,
	scope: { id: 'app-settings' },
	mutationFn: async (enabled: boolean) => {
		const settings = await queryClient.fetchQuery(appSettingsQueryOptions())
		const nextSettings = { ...settings, developer_mode: enabled }
		await set(nextSettings)
		return nextSettings
	},
	onMutate: () => queryClient.cancelQueries({ queryKey: appSettingsKeys.all }),
	onSuccess: (settings) => {
		const selectedTab = modal.value ? availableTabs.value[modal.value.selectedTab] : undefined

		queryClient.setQueryData(appSettingsKeys.all, settings)
		appSettings.devMode = settings.developer_mode

		if (modal.value) {
			const selectedTabIndex = selectedTab ? availableTabs.value.indexOf(selectedTab) : -1
			modal.value.setTab(selectedTabIndex >= 0 ? selectedTabIndex : 0)
		}
	},
	onError: handleError,
	onSettled: () => queryClient.invalidateQueries({ queryKey: appSettingsKeys.all }),
})

function devModeCount() {
	if (developerModeMutation.isPending.value) return
	devModeCounter.value++
	if (devModeCounter.value > 5) {
		devModeCounter.value = 0
		developerModeMutation.mutate(!appSettings.devMode)
	}
}

const messages = defineMessages({
	downloading: {
		id: 'app.settings.downloading',
		defaultMessage: 'Downloading v{version}',
	},
	appVersion: {
		id: 'app.settings.app-version',
		defaultMessage: 'Modrinth App {version}',
	},
	macos: {
		id: 'app.settings.operating-system.macos',
		defaultMessage: 'macOS',
	},
	developerModeButtonLabel: {
		id: 'app.settings.developer-mode-button.label',
		defaultMessage: 'Toggle developer mode',
	},
})
</script>
<template>
	<TabbedModal
		ref="modal"
		:tabs="availableTabs"
		width="928px"
		max-width="928px"
		:before-hide="canLeaveCurrentTab"
		:before-tab-change="canLeaveCurrentTab"
		:floating-action-bar-shown="hasUnsavedChanges"
	>
		<template #title>
			<span class="text-2xl font-semibold text-contrast">
				{{ formatMessage(commonMessages.settingsLabel) }}
			</span>
		</template>
		<template #floating-action-bar>
			<UnsavedChangesPopup
				ref="unsavedChangesPopup"
				:original="originalUnsavedChangesState"
				:modified="modifiedUnsavedChangesState"
				:saving="savingUnsavedChanges"
				inline
				@reset="resetUnsavedChanges"
				@save="saveUnsavedChanges"
			/>
		</template>
		<template #footer>
			<div class="mt-auto text-secondary text-sm">
				<div class="mb-3">
					<template v-if="progress > 0 && progress < 1">
						<p class="m-0 mb-2">
							{{ formatMessage(messages.downloading, { version: downloadingVersion }) }}
						</p>
						<ProgressBar :progress="progress" />
					</template>
				</div>
				<p v-if="appSettings.devMode" class="text-brand font-semibold m-0 mb-2">
					{{ formatMessage(developerModeEnabled) }}
				</p>
				<div class="flex items-center gap-3">
					<button
						:aria-label="formatMessage(messages.developerModeButtonLabel)"
						:disabled="developerModeMutation.isPending.value"
						class="p-0 m-0 bg-transparent border-none cursor-pointer button-animation"
						:class="{
							'text-brand': appSettings.devMode,
							'text-secondary': !appSettings.devMode,
						}"
						@click="devModeCount"
					>
						<ModrinthIcon aria-hidden="true" class="w-6 h-6" />
					</button>
					<div v-if="appInfo" class="max-w-[200px]">
						<p class="m-0">
							{{ formatMessage(messages.appVersion, { version: appInfo.version }) }}
						</p>
						<p class="m-0">
							<span v-if="appInfo.osPlatform === 'macos'">{{ formatMessage(messages.macos) }}</span>
							<span v-else class="capitalize">{{ appInfo.osPlatform }}</span>
							{{ appInfo.osVersion }}
						</p>
					</div>
				</div>
			</div>
		</template>
	</TabbedModal>
</template>
