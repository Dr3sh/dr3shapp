<script setup>
import { ModrinthApiError } from '@modrinth/api-client'
import {
	ArrowBigUpDashIcon,
	ArrowLeftRightIcon,
	CompassIcon,
	ImageIcon,
	LogInIcon,
	LogOutIcon,
	PlayIcon,
	PlusIcon,
	RefreshCwIcon,
	ServerStackIcon,
	SettingsIcon,
	ShirtIcon,
	SpinnerIcon,
	ToggleRightIcon,
	UserIcon,
	UserPlusIcon,
	XIcon,
} from '@modrinth/assets'
import {
	AccountSwitchOverlay,
	Admonition,
	Avatar,
	commonMessages,
	commonSettingsMessages,
	ContentInstallModal,
	ContentUpdaterModal,
	CreationFlowModal,
	defineMessages,
	I18nDebugPanel,
	IconButton,
	LoadingBar,
	NotificationPanel,
	PopupNotificationPanel,
	provideFileDownload,
	provideLoadingState,
	provideModalBehavior,
	provideModrinthClient,
	provideNotificationManager,
	providePageContext,
	providePopupNotificationManager,
	TeleportOverflowMenu,
	TooltipDirective,
	useDebugLogger,
	useFormatBytes,
	useHostingIntercom,
	UserRoleIcon,
	useVIntl,
} from '@modrinth/ui'
import { renderString } from '@modrinth/utils/parse'
import { useQueries, useQuery, useQueryClient } from '@tanstack/vue-query'
import { getVersion } from '@tauri-apps/api/app'
import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { fetch as tauriFetch } from '@tauri-apps/plugin-http'
import { openUrl } from '@tauri-apps/plugin-opener'
import { type } from '@tauri-apps/plugin-os'
import { saveWindowState, StateFlags } from '@tauri-apps/plugin-window-state'
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'

import { useRouteLoading } from '@/app/runtime/use-route-loading'
import AppSidebar from '@/app/shell/sidebar/index.vue'
import AppTitleBar from '@/app/shell/title-bar/index.vue'
import Dr3shRail from '@/components/ui/Dr3shRail.vue'
import Dr3shTopBar from '@/components/ui/Dr3shTopBar.vue'
import DeveloperMenu from '@/components/ui/DeveloperMenu.vue'
import {
	activeAccount,
	currentView,
	isDeveloper,
	isOffline,
	catalogError,
	retryingCatalog,
	maintenanceMode,
	maintenanceMessage,
	refreshAccount,
	servers,
	selectedId,
	fetchVersionsData,
	deselectModpack,
	startupBlockedOffline,
	connectionLostFatal,
	debugPreview,
	resetDebugPreview,
	checkNetworkHealthTick,
} from '@/composables/use-dr3sh-launcher'
import {
	initDr3shBackendState,
	getInstalledVersion,
	getInstalledFolder,
	getInstalledInstanceId,
	findMatchingInstance,
} from '@/composables/use-dr3sh-downloader'
import { trackExternalFileDownload } from '@/components/ui/download-manager/external-file-downloads'
import ErrorModal from '@/components/ui/ErrorModal.vue'
import HostingUpdateRequired from '@/components/ui/HostingUpdateRequired.vue'
import AddServerToInstanceModal from '@/components/ui/install_flow/AddServerToInstanceModal.vue'
import UnknownPackWarningModal from '@/components/ui/install_flow/UnknownPackWarningModal.vue'
import IconEditorModal from '@/components/ui/instance_settings/InstanceIconEditorModal.vue'
import MinecraftAuthErrorModal from '@/components/ui/minecraft-auth-error-modal/MinecraftAuthErrorModal.vue'
import MinecraftRequiredModal from '@/components/ui/minecraft-required-modal/MinecraftRequiredModal.vue'
import InstallToPlayModal from '@/components/ui/modal/InstallToPlayModal.vue'
import ModpackAlreadyInstalledModal from '@/components/ui/modal/ModpackAlreadyInstalledModal.vue'
import ModrinthAccountRequiredModal from '@/components/ui/modal/ModrinthAccountRequiredModal.vue'
import UpdateToPlayModal from '@/components/ui/modal/UpdateToPlayModal.vue'
import NavButton from '@/components/ui/NavButton.vue'
import QuickInstanceSwitcher from '@/components/ui/QuickInstanceSwitcher.vue'
import SharedInstanceInviteHandler from '@/components/ui/shared-instances/shared-instance-invite-handler/index.vue'
import SplashScreen from '@/components/ui/SplashScreen.vue'
import SurveyPopup from '@/components/ui/SurveyPopup.vue'
import { useCheckDisableMouseover } from '@/composables/macCssFix.js'
import { useAppEvent } from '@/composables/use-app-event'
import { useAppSettings } from '@/composables/use-app-settings.ts'
import { useError } from '@/composables/use-error.js'
import { useInstanceMetadataRefresh } from '@/composables/use-instance-metadata-refresh'
import { useQuickInstanceLimit } from '@/composables/use-quick-instance-limit.ts'
import { isDarkTheme, useTheme } from '@/composables/use-theme.ts'
import { config } from '@/config'
import { provideMinecraftAccounts } from '@/features/minecraft-accounts/context'
import AppSettingsModal from '@/features/settings/app-settings-modal/index.vue'
import { provideSettingsActions } from '@/features/settings/settings-actions'
import NewUpdateModal from '@/features/updates/new-update-modal/index.vue'
import { getAccountAppearance, rememberAccountAppearance } from '@/helpers/account-appearance.ts'
import {
	hide_ads_window,
	init_ads_window,
	perform_ads_consent_action,
	release_ads_window_hold,
	should_show_ads_consent_popup,
	take_ads_window_hold,
} from '@/helpers/ads.js'
import { debugAnalytics, initAnalytics, trackEvent } from '@/helpers/analytics'
import { check_reachable } from '@/helpers/auth.js'
import { get_user, get_user_many, get_version } from '@/helpers/cache.js'
import { gameSettingsQueryOptions } from '@/helpers/game-options'
import { install_create_modpack_instance, install_get_modpack_preview } from '@/helpers/install'
import {
	can_current_user_use_shared_instances,
	get as getInstance,
	get_full_path,
	list as listInstances,
	run,
	set_global_synced_option,
} from '@/helpers/instance'
import { maxMemoryQueryOptions } from '@/helpers/jre.js'
import {
	get as getCreds,
	getAll as getAllCreds,
	login,
	logout,
	removeUser,
	setActive,
} from '@/helpers/mr_auth.ts'
import { mergeUrlQuery, parseModrinthLink } from '@/helpers/project-links.ts'
import {
	appSettingsKeys,
	appSettingsQueryOptions,
	get as getSettings,
	set as setSettings,
} from '@/helpers/settings.ts'
import { debugStartup, traceStartupStep } from '@/helpers/startup-debug'
import { get_opening_command, initialize_state } from '@/helpers/state'
import {
	gameOptionsSyncSourcesQueryOptions,
	globalSyncedOptionsQueryOptions,
	initializedSyncedOptionsQueryOptions,
	syncedOptionsKeys,
	syncedServersQueryOptions,
} from '@/helpers/synced-options'
import { syncedPackQueryOptions } from '@/helpers/synced-packs'
import { hasActivePride26Midas, hasMidasBadge } from '@/helpers/user-campaigns.ts'
import { get_user_preferences } from '@/helpers/user-preferences.ts'
import { parse_modrinth_user_link } from '@/helpers/users'
import {
	getOS,
	isDev,
} from '@/helpers/utils.js'
import { start_join_server, start_join_singleplayer_world } from '@/helpers/worlds.ts'
import i18n, { setLocale } from '@/i18n.config'
import {
	instanceKeys,
	instanceListQueryOptions,
	screenshotKeys,
} from '@/pages/instance/query-options'
import { createFileDownload } from '@/platform/adapters/file-download'
import { createAppClient } from '@/platform/modrinth-client'
import {
	appUpdateState,
	markAppUpdateActionable,
	setAppUpdateActions,
} from '@/providers/app-update.ts'
import { createContentInstall, provideContentInstall } from '@/providers/content-install'
import {
	provideAppUpdateDownloadProgress,
} from '@/providers/download-progress.ts'
import { createServerInstall, provideServerInstall } from '@/providers/server-install'
import { setupProviders } from '@/providers/setup'
import { setupAppEventsProvider } from '@/providers/setup/app-events'
import { setupAuthProvider } from '@/providers/setup/auth'
import { setupAppUserPreferencesProvider } from '@/providers/setup/user-preferences.ts'
import { createBreadcrumbManager, provideBreadcrumbManager } from '@/shared/breadcrumbs'
import { appMessages } from '@/utils/app-messages'

import { AppNotificationManager } from './providers/app-notifications'
import { AppPopupNotificationManager } from './providers/app-popup-notifications'

debugStartup('App setup entered')
const appSettings = useAppSettings()
const appTheme = useTheme()
const quickInstances = useQuickInstanceLimit()
const router = useRouter()
const route = useRoute()
const { channel: appEventChannel, events: appEvents } = setupAppEventsProvider()
useInstanceMetadataRefresh(appEvents)
const breadcrumbManager = createBreadcrumbManager()
provideBreadcrumbManager(breadcrumbManager)
let fullscreenAdsWindowHold = false

async function handleFullscreenChange() {
	const fullscreen = document.fullscreenElement !== null
	if (fullscreen === fullscreenAdsWindowHold) return

	fullscreenAdsWindowHold = fullscreen
	try {
		if (fullscreen) {
			await take_ads_window_hold()
		} else {
			await release_ads_window_hold()
		}
	} catch (error) {
		fullscreenAdsWindowHold = !fullscreen
		handleError(error)
	}
}

const APP_LEFT_NAV_WIDTH = '4rem'
const APP_SIDEBAR_WIDTH = 300
const INTERCOM_BUBBLE_DEFAULT_PADDING = 20
const credentials = ref()
const storedModrinthAccounts = ref([])
let credentialsRefreshId = 0
const sidebarToggled = ref(true)
watch(
	() => appSettings.toggleSidebar,
	(toggleSidebar) => {
		sidebarToggled.value = !toggleSidebar
	},
)
const forceSidebar = computed(
	() =>
		route.path.startsWith('/browse') ||
		route.path.startsWith('/project') ||
		route.path.startsWith('/user'),
)
const sidebarVisible = computed(() => false)
const hostingRouteActive = computed(() => route.path.startsWith('/hosting'))
const hostingUpdateRequired = computed(
	() =>
		hostingRouteActive.value &&
		!!appUpdateState.availableUpdate.value &&
		appUpdateState.updatesEnabled.value,
)
const hostingIntercomIdentityKey = computed(() => {
	const rawServerId = route.params.id
	const serverId = Array.isArray(rawServerId) ? rawServerId[0] : rawServerId
	const userId = credentials.value?.user_id ?? credentials.value?.user?.id ?? 'anonymous'
	return `${userId}:${serverId ?? 'hosting'}`
})
const hostingIntercom = useHostingIntercom({
	enabled: computed(
		() => hostingRouteActive.value && !hostingUpdateRequired.value && !!credentials.value?.session,
	),
	appId: 'ykeritl9',
	fetchToken: fetchIntercomToken,
	identityKey: hostingIntercomIdentityKey,
	horizontalPadding: computed(() =>
		sidebarVisible.value
			? APP_SIDEBAR_WIDTH + INTERCOM_BUBBLE_DEFAULT_PADDING
			: INTERCOM_BUBBLE_DEFAULT_PADDING,
	),
})

const notificationManager = new AppNotificationManager()
provideNotificationManager(notificationManager)
const { handleError, addNotification } = notificationManager

useAppEvent(
	'warning',
	(event) =>
		addNotification({
			title: formatMessage(messages.warning),
			text: event.message,
			type: 'warning',
		}),
	appEvents,
)

const popupNotificationManager = new AppPopupNotificationManager()
providePopupNotificationManager(popupNotificationManager)
const { addPopupNotification } = popupNotificationManager
let adsConsentPopupId = null
useAppEvent('ads_consent_required', handleAdsConsentRequired, appEvents)

const tauriApiClient = createAppClient()
provideModrinthClient(tauriApiClient)
provideFileDownload(createFileDownload(tauriApiClient, trackExternalFileDownload))
const { data: authenticatedModrinthUser } = useQuery({
	queryKey: computed(() => ['authenticated-user', 'campaigns', credentials.value?.user?.id]),
	queryFn: () => tauriApiClient.labrinth.users_v3.getAuthenticated(),
	enabled: () => !!credentials.value?.session,
	retry: false,
})
useQuery({
	queryKey: computed(() => instanceKeys.sharedEligibility(credentials.value?.user?.id)),
	queryFn: can_current_user_use_shared_instances,
	enabled: () => !!credentials.value?.session && !!credentials.value?.user?.id,
	retry: false,
	staleTime: Infinity,
	refetchOnMount: false,
	refetchOnWindowFocus: false,
	refetchOnReconnect: false,
})
const hasPlus = computed(
	() =>
		!!credentials.value?.user &&
		(hasMidasBadge(credentials.value.user) ||
			hasActivePride26Midas(authenticatedModrinthUser.value?.campaigns?.pride_26)),
)
const showAd = computed(
	() => sidebarVisible.value && !hasPlus.value && credentials.value !== undefined,
)
const adConsentAvailable = computed(() => credentials.value !== undefined && !hasPlus.value)
providePageContext({
	hierarchicalSidebarAvailable: ref(true),
	showAds: showAd,
	adConsentAvailable,
	floatingActionBarOffsets: {
		left: ref(APP_LEFT_NAV_WIDTH),
		right: computed(() => (sidebarVisible.value ? `${APP_SIDEBAR_WIDTH}px` : '0px')),
	},
	intercomBubble: hostingIntercom.intercomBubble,
	featureFlags: {
		serverRamAsBytesAlwaysOn: computed(() =>
			appSettings.getFeatureFlag('server_ram_as_bytes_always_on'),
		),
	},
	openExternalUrl: (url) => void openUrl(url),
})
provideModalBehavior({
	noblur: computed(() => !appTheme.advancedRendering),
	onShow: () => take_ads_window_hold(),
	onHide: () => release_ads_window_hold(),
})

const creationIconEditorModal = ref(null)
const creationGeneratedIcon = ref(null)
const creationIconTarget = ref('creation-flow')

const {
	installationModal,
	unknownPackWarningModal,
	fetchExistingInstanceNames,
	handleCreate,
	handleBrowseModpacks,
	searchProjects,
	getLoaderManifest,
	setModpackAlreadyInstalledModal,
	handleModpackDuplicateCreateAnyway,
	handleModpackDuplicateGoToInstance,
	onboardingChecklist,
	tags,
} = setupProviders(
	tauriApiClient,
	notificationManager,
	popupNotificationManager,
	appEvents,
	(iconPath) =>
		creationGeneratedIcon.value?.path === iconPath ? creationGeneratedIcon.value.config : null,
)

async function randomizeCreationIcon() {
	const generated = await creationIconEditorModal.value?.randomizeAndSave()
	if (!generated) return null

	creationGeneratedIcon.value = { path: generated.iconPath, config: generated.config }
	return {
		path: generated.iconPath,
		previewUrl: convertFileSrc(generated.iconPath),
	}
}

function customizeCreationIcon() {
	creationIconTarget.value = 'creation-flow'
	creationIconEditorModal.value?.show()
}

function customizeContentInstallIcon() {
	creationIconTarget.value = 'content-install'
	creationIconEditorModal.value?.show()
}

function onCreationIconSaved(iconPath, config) {
	creationGeneratedIcon.value = { path: iconPath, config }
	if (creationIconTarget.value === 'content-install') {
		modInstallModal.value?.setIcon(iconPath, convertFileSrc(iconPath))
		return
	}

	const context = installationModal.value?.ctx
	if (!context) return

	context.instanceIcon.value = null
	context.instanceIconUrl.value = convertFileSrc(iconPath)
	context.instanceIconPath.value = iconPath
}

const displayedServerInviteNotifications = new Set()
const serverInvitePopupNotificationIds = new Set()
let liveNotificationGeneration = 0
let liveNotificationsEnabled = true

const offline = ref(!navigator.onLine)
window.addEventListener('offline', () => {
	offline.value = true
})
window.addEventListener('online', () => {
	offline.value = false
	void authServerQuery.refetch()
	if (!availableUpdate.value) {
		void checkUpdates()
	}
})

const os = ref('')
const isDevEnvironment = ref(false)

const stateInitialized = ref(false)
const globalSyncedOptionsQuery = useQuery({
	...globalSyncedOptionsQueryOptions(),
	enabled: computed(() => stateInitialized.value),
})
useQueries({
	queries: computed(() =>
		[
			appSettingsQueryOptions(),
			instanceListQueryOptions(),
			maxMemoryQueryOptions(),
			gameSettingsQueryOptions(),
			initializedSyncedOptionsQueryOptions(),
			gameOptionsSyncSourcesQueryOptions(),
			syncedServersQueryOptions(),
			syncedPackQueryOptions('resourcepack'),
			syncedPackQueryOptions('datapack'),
		].map((options) => ({
			...options,
			enabled: stateInitialized.value && options.enabled !== false,
		})),
	),
})

const criticalErrorMessage = ref()

const isMaximized = ref(false)
const isFullscreen = ref(false)

watch([os, isFullscreen], ([osName, fullscreen]) => {
	document.documentElement.classList.toggle('mac-traffic-lights', osName === 'MacOS' && !fullscreen)
})

const authUnreachableDebug = useDebugLogger('AuthReachableChecker')
const authServerQuery = useQuery({
	queryKey: ['authServerReachability'],
	queryFn: async () => {
		await check_reachable()
		authUnreachableDebug('Auth servers are reachable')
		return true
	},
	refetchInterval: 5 * 60 * 1000, // 5 minutes
	retry: false,
	refetchOnWindowFocus: false,
})

const authUnreachable = computed(() => {
	if (offline.value || isOffline.value || !activeAccount.value?.isMicrosoft) {
		return false
	}
	if (authServerQuery.isError.value && !authServerQuery.isLoading.value) {
		console.warn('Failed to reach auth servers', authServerQuery.error.value)
		return true
	}
	return false
})

let unlistenEditMenu

function handleEditMenuAction(action) {
	const event = new CustomEvent(`edit-menu:${action}`, { cancelable: true })
	if (document.dispatchEvent(event)) document.execCommand(action)
}

onMounted(async () => {
	try {
		const listeners = await Promise.all([
			listen('edit-menu://undo', () => handleEditMenuAction('undo')),
			listen('edit-menu://redo', () => handleEditMenuAction('redo')),
		])
		unlistenEditMenu = () => listeners.forEach((unlisten) => unlisten())
	} catch (error) {
		handleError(error)
	}

	await useCheckDisableMouseover()
	try {
		handleAdsConsentRequired(await should_show_ads_consent_popup())
	} catch (error) {
		handleError(error)
	}

	document.querySelector('body').addEventListener('click', handleClick)
	document.querySelector('body').addEventListener('auxclick', handleAuxClick)
	document.querySelector('body').addEventListener('contextmenu', handleContextMenu)
	document.addEventListener('fullscreenchange', handleFullscreenChange)

	checkUpdates()
})

onUnmounted(async () => {
	document.querySelector('body').removeEventListener('click', handleClick)
	document.querySelector('body').removeEventListener('auxclick', handleAuxClick)
	document.querySelector('body').removeEventListener('contextmenu', handleContextMenu)
	document.removeEventListener('fullscreenchange', handleFullscreenChange)
	unlistenEditMenu?.()
	clearDelayedUpdatePopup()
	if (launcherUpdateCheckTimer !== null) {
		clearTimeout(launcherUpdateCheckTimer)
		launcherUpdateCheckTimer = null
	}

	if (fullscreenAdsWindowHold) {
		fullscreenAdsWindowHold = false
		await release_ads_window_hold().catch(handleError)
	}
	await unlistenUpdateDownload?.()
})

const { formatMessage } = useVIntl()
const formatBytes = useFormatBytes()

const messages = defineMessages({
	warning: { id: 'app.notification.warning', defaultMessage: 'Warning' },
	updateDownloadMissingVersion: {
		id: 'app.update.download-error.missing-version',
		defaultMessage: 'Failed to download update: no version available',
	},
	updateInstalledToastTitle: {
		id: 'app.update.complete-toast.title',
		defaultMessage: 'Version {version} was successfully installed!',
	},
	updateInstalledToastText: {
		id: 'app.update.complete-toast.text',
		defaultMessage: 'Click here to view the changelog.',
	},
	authUnreachableHeader: {
		id: 'app.auth-servers.unreachable.header',
		defaultMessage: 'No se puede conectar con los servidores de autenticación de Minecraft',
	},
	authUnreachableBody: {
		id: 'app.auth-servers.unreachable.body',
		defaultMessage:
			'Los servidores de Microsoft/Mojang pueden estar temporalmente inaccesibles. Aún puedes jugar en modo sin conexión.',
	},
	adsConsentTitle: {
		id: 'app.ads-consent.title',
		defaultMessage: 'Your privacy and how ads support Dr3sh App',
	},
	adsConsentBody: {
		id: 'app.ads-consent.body',
		defaultMessage:
			'Ads make Dr3sh App possible and fund creator payouts. Our partners may store or access cookies in the app to personalize ads and measure performance.',
	},
	adsConsentManage: {
		id: 'app.ads-consent.manage',
		defaultMessage: 'Manage preferences',
	},
	adsConsentReject: {
		id: 'app.ads-consent.reject',
		defaultMessage: 'Reject all',
	},
	adsConsentAccept: {
		id: 'app.ads-consent.accept',
		defaultMessage: 'Accept all',
	},
	home: {
		id: 'app.nav.home',
		defaultMessage: 'Home',
	},
	modrinthHosting: {
		id: 'app.nav.modrinth-hosting',
		defaultMessage: 'Dr3sh App Hosting',
	},
	screenshots: {
		id: 'app.nav.screenshots',
		defaultMessage: 'Screenshots',
	},
	createNewInstance: {
		id: 'app.nav.create-new-instance',
		defaultMessage: 'Create new instance',
	},
	modrinthAccount: {
		id: 'app.nav.modrinth-account',
		defaultMessage: 'Dr3sh App account',
	},
	viewProfile: {
		id: 'app.nav.view-profile',
		defaultMessage: 'View profile',
	},
	addFriend: {
		id: 'friends.action.add-friend',
		defaultMessage: 'Add a friend',
	},
	signInToModrinthAccount: {
		id: 'app.nav.sign-in-to-modrinth-account',
		defaultMessage: 'Sign into Dr3sh App',
	},
	loadingProfile: {
		id: 'app.nav.loading-profile',
		defaultMessage: 'Loading profile...',
	},
	switchAccount: {
		id: 'app.nav.switch-account',
		defaultMessage: 'Switch account',
	},
	addAccount: {
		id: 'app.nav.add-account',
		defaultMessage: 'Add account',
	},
	removeAccount: {
		id: 'app.nav.remove-account',
		defaultMessage: 'Remove account',
	},
	restarting: {
		id: 'app.restarting',
		defaultMessage: 'Restarting...',
	},
	upgradeToModrinthPlus: {
		id: 'app.nav.upgrade-to-modrinth-plus',
		defaultMessage: 'Upgrade to Dr3sh App+',
	},
})

function handleAdsConsentRequired(required) {
	if (!required) {
		if (adsConsentPopupId !== null) {
			popupNotificationManager.removeNotification(adsConsentPopupId)
			adsConsentPopupId = null
		}
		return
	}

	if (
		adsConsentPopupId !== null &&
		popupNotificationManager.getNotifications().some((item) => item.id === adsConsentPopupId)
	) {
		return
	}

	const notification = addPopupNotification({
		contentType: 'standard',
		title: formatMessage(messages.adsConsentTitle),
		text: formatMessage(messages.adsConsentBody),
		type: 'info',
		hideIcon: true,
		autoCloseMs: null,
		dismissible: false,
		buttons: [
			{
				label: formatMessage(messages.adsConsentManage),
				action: () => perform_ads_consent_action('manage').catch(handleError),
				color: 'standard',
				keepOpen: true,
			},
			{
				label: formatMessage(messages.adsConsentReject),
				action: () => perform_ads_consent_action('reject').catch(handleError),
				color: 'brand',
				keepOpen: true,
			},
			{
				label: formatMessage(messages.adsConsentAccept),
				action: () => perform_ads_consent_action('accept').catch(handleError),
				color: 'brand',
				keepOpen: true,
			},
		],
	})

	adsConsentPopupId = notification.id
}

async function setupApp() {
	tags.initialize()
	await traceStartupStep('Initialize onboarding checklist', () => onboardingChecklist.initialize())

	const {
		native_decorations,
		theme,
		locale,
		telemetry,
		hide_nametag_skins_page,
		advanced_rendering,
		toggle_sidebar,
		sync_theme_across_devices,
		sync_behavior_across_devices,
		sync_features_across_devices,
		show_files_tab_in_instances,
		show_worlds_tab_in_instances,
		show_screenshots_tab_in_instances,
		show_skin_selector_in_sidebar,
		refocus_on_game_close,
		compact_instance_cards,
		show_play_time,
		warn_on_unknown_modpacks,
		skip_non_essential_warnings,
		show_jump_in,
		always_show_copy_details,
		hide_installed_modpacks,
		advanced_filters_collapsed,
		dismissed_photosensitivity_filter_warning,
		friends_active_collapsed,
		friends_online_collapsed,
		friends_offline_collapsed,
		friends_pending_collapsed,
		developer_mode,
		feature_flags,
		pending_update_toast_for_version,
	} = await traceStartupStep('Read startup settings', getSettings)

	// Initialize locale from saved settings
	if (locale) {
		await traceStartupStep('Apply startup locale', () => setLocale(locale))
	}

	Object.assign(appSettings.featureFlags, feature_flags)
	isMaximized.value = await traceStartupStep('Read window maximized state', () =>
		getCurrentWindow().isMaximized(),
	)
	isFullscreen.value = await traceStartupStep('Read window fullscreen state', () =>
		getCurrentWindow().isFullscreen(),
	)
	os.value = await traceStartupStep('Read operating system', getOS)
	const dev = await traceStartupStep('Read development mode', isDev)
	isDevEnvironment.value = dev
	const version = await traceStartupStep('Read app version', getVersion)
	appSettings.nativeDecorations = native_decorations
	if (os.value !== 'MacOS') {
		await traceStartupStep('Apply window decorations', () =>
			getCurrentWindow().setDecorations(native_decorations),
		)
	}

	appTheme.preferred = theme
	appTheme.advancedRendering = advanced_rendering
	appTheme.syncAcrossDevices = sync_theme_across_devices
	appSettings.syncBehaviorAcrossDevices = sync_behavior_across_devices
	appSettings.syncFeaturesAcrossDevices = sync_features_across_devices
	appSettings.hideNametagSkinsPage = hide_nametag_skins_page
	appSettings.toggleSidebar = toggle_sidebar
	appSettings.showFilesTabInInstances = show_files_tab_in_instances
	appSettings.showWorldsTabInInstances = show_worlds_tab_in_instances
	appSettings.showScreenshotsTabInInstances = show_screenshots_tab_in_instances
	appSettings.showSkinSelectorInSidebar = show_skin_selector_in_sidebar
	appSettings.refocusOnGameClose = refocus_on_game_close
	appSettings.compactInstanceCards = compact_instance_cards
	appSettings.showPlayTime = show_play_time
	appSettings.warnOnUnknownModpacks = warn_on_unknown_modpacks
	appSettings.skipNonEssentialWarnings = skip_non_essential_warnings
	appSettings.showJumpIn = show_jump_in
	appSettings.alwaysShowCopyDetails = always_show_copy_details
	appSettings.hideInstalledModpacks = hide_installed_modpacks
	appSettings.advancedFiltersCollapsed = advanced_filters_collapsed
	appSettings.dismissedPhotosensitivityFilterWarning = dismissed_photosensitivity_filter_warning
	appSettings.friendsActiveCollapsed = friends_active_collapsed
	appSettings.friendsOnlineCollapsed = friends_online_collapsed
	appSettings.friendsOfflineCollapsed = friends_offline_collapsed
	appSettings.friendsPendingCollapsed = friends_pending_collapsed
	appSettings.devMode = developer_mode
	stateInitialized.value = true
	debugStartup('App state initialized')
	await traceStartupStep('Render initialized app', nextTick)

	await traceStartupStep('Register window resize listener', () =>
		getCurrentWindow().onResized(async () => {
			isMaximized.value = await getCurrentWindow().isMaximized()
			isFullscreen.value = await getCurrentWindow().isFullscreen()
		}),
	)

	if (telemetry) {
		initAnalytics()
		if (dev) debugAnalytics()
		trackEvent('Launched', { version, dev })
	}

	const osType = await traceStartupStep('Read operating system type', async () => type())
	if (osType === 'macos') {
		document.getElementsByTagName('html')[0].classList.add('mac')
	} else {
		document.getElementsByTagName('html')[0].classList.add('windows')
	}

	traceStartupStep('Read opening command', get_opening_command).then(handleCommand)
	traceStartupStep('Refresh startup credentials', fetchCredentials)

	if (pending_update_toast_for_version !== null) {
		const settings = await traceStartupStep(
			'Read settings to clear update notification',
			getSettings,
		)
		settings.pending_update_toast_for_version = null
		await traceStartupStep('Clear update notification', () => setSettings(settings))
	}
}

const stateFailed = ref(false)
const { loading, canNavigateBack, canNavigateForward, onSuspensePending, onSuspenseResolve } =
	useRouteLoading({ stateInitialized, stateFailed })
provideLoadingState(loading)

traceStartupStep('Initialize backend state', () => initialize_state(appEventChannel))
	.then(() => {
		traceStartupStep('Initialize frontend state', setupApp).catch((err) => {
			stateFailed.value = true
			console.error(err)
			error.showError(err, null, false, 'state_init')
		})
	})
	.catch((err) => {
		stateFailed.value = true
		console.error('Failed to initialize app', err)
		error.showError(err, null, false, 'state_init')
	})

const handleClose = async () => {
	await saveWindowState(StateFlags.ALL)
	await getCurrentWindow().close()
}

const queryClient = useQueryClient()

watch([stateInitialized, () => credentials.value?.session], ([ready, session]) => {
	if (ready && session) {
		queryClient.prefetchQuery({
			queryKey: ['servers'],
			queryFn: async () => {
				const response = await tauriApiClient.archon.servers_v0.list({ limit: 100 })
				const hasMedalServers = response.servers.some((s) => s.is_medal)
				if (hasMedalServers) {
					const subscriptions = await tauriApiClient.labrinth.billing_internal.getSubscriptions()
					for (const server of response.servers) {
						if (server.is_medal) {
							const sub = subscriptions.find((s) => s.metadata?.id === server.server_id)
							if (sub) {
								server.medal_expires = new Date(
									new Date(sub.created).getTime() + 5 * 86400000,
								).toISOString()
							}
						}
					}
				}
				return response
			},
			staleTime: 30_000,
		})
		queryClient.prefetchQuery({
			queryKey: ['billing', 'subscriptions'],
			queryFn: () => tauriApiClient.labrinth.billing_internal.getSubscriptions(),
			staleTime: 30_000,
		})
		queryClient.prefetchQuery({
			queryKey: ['billing', 'payments'],
			queryFn: () => tauriApiClient.labrinth.billing_internal.getPayments(),
			staleTime: 30_000,
		})
	}
})

const error = useError()
const errorModal = ref()
const minecraftAuthErrorModal = ref()
const minecraftRequiredModal = ref()

const contentInstall = createContentInstall({ router, handleError, appEvents })
provideContentInstall(contentInstall)
const {
	instances: contentInstallInstances,
	compatibleLoaders: contentInstallLoaders,
	gameVersions: contentInstallGameVersions,
	loading: contentInstallLoading,
	defaultTab: contentInstallDefaultTab,
	preferredLoader: contentInstallPreferredLoader,
	preferredGameVersion: contentInstallPreferredGameVersion,
	releaseGameVersions: contentInstallReleaseGameVersions,
	projectInfo: contentInstallProjectInfo,
	handleInstallToInstance,
	handleCreateAndInstall,
	prepareNewInstance,
	handleNavigate: handleContentInstallNavigate,
	handleCancel: handleContentInstallCancel,
	setContentInstallModal,
	setModpackAlreadyInstalledModal: setContentInstallModpackAlreadyInstalledModal,
	handleModpackDuplicateCreateAnyway: handleContentInstallModpackDuplicateCreateAnyway,
	handleModpackDuplicateGoToInstance: handleContentInstallModpackDuplicateGoToInstance,
	setIncompatibilityWarningModal: setContentIncompatibilityWarningModal,
	incompatibilityWarningVersions: contentInstallIncompatibilityWarningVersions,
	incompatibilityWarningCurrentGameVersion: contentInstallIncompatibilityWarningCurrentGameVersion,
	incompatibilityWarningCurrentLoader: contentInstallIncompatibilityWarningCurrentLoader,
	incompatibilityWarningProjectType: contentInstallIncompatibilityWarningProjectType,
	incompatibilityWarningProjectIconUrl: contentInstallIncompatibilityWarningProjectIconUrl,
	incompatibilityWarningProjectName: contentInstallIncompatibilityWarningProjectName,
	incompatibilityWarningMessage: contentInstallIncompatibilityWarningMessage,
	incompatibilityWarningInstalling: contentInstallIncompatibilityWarningInstalling,
	handleIncompatibilityWarningInstall: handleContentInstallIncompatibilityWarningInstall,
	handleIncompatibilityWarningCancel: handleContentInstallIncompatibilityWarningCancel,
} = contentInstall

async function prepareCreationProjectInstall(projectId, projectType) {
	if (projectType === 'modpack') {
		await contentInstall.install(
			projectId,
			null,
			null,
			'CreationModalProject',
			undefined,
			(instanceId) => void router.push(`/instance/${encodeURIComponent(instanceId)}`),
		)
		return null
	}

	await prepareNewInstance(projectId)
	const info = contentInstallProjectInfo.value
	if (!info) throw new Error(`Project information is unavailable: '${projectId}'`)

	return {
		projectId,
		title: info.title,
		iconUrl: info.iconUrl,
		link: info.link,
		owner: info.owner,
		compatibleLoaders: [...contentInstallLoaders.value],
		gameVersions: [...contentInstallGameVersions.value],
		releaseGameVersions: new Set(contentInstallReleaseGameVersions.value),
	}
}

const serverInstall = createServerInstall({
	router,
	handleError,
	popupNotificationManager,
	appEvents,
})
provideServerInstall(serverInstall)
const {
	setInstallToPlayModal: setServerInstallToPlayModal,
	setUpdateToPlayModal: setServerUpdateToPlayModal,
	setAddServerToInstanceModal: setServerAddServerToInstanceModal,
	playServerProject,
} = serverInstall

const modInstallModal = ref()
const modpackAlreadyInstalledModal = ref()
const contentInstallModpackAlreadyInstalledModal = ref()
const addServerToInstanceModal = ref()
const incompatibilityWarningModal = ref()
const installToPlayModal = ref()
const sharedInstanceInviteHandler = ref()
const updateToPlayModal = ref()

const modrinthLoginModal = ref()
const appSettingsModal = ref()
const newUpdateModal = ref()

const settingsActions = provideSettingsActions({
	openSettings: (tab) => {
		if (tab === 'accounts') {
			appSettingsModal.value?.showAccounts()
		} else {
			appSettingsModal.value?.show()
		}
	},
	openProfile: () => appSettingsModal.value?.showAccounts(),
	openAccounts: () => appSettingsModal.value?.showAccounts(),
	openFeatureFlags: () => appSettingsModal.value?.showFeatureFlags(),
	openSyncedOptions: () => appSettingsModal.value?.showSyncedOptions(),
	openInstanceDefaults: (tab) => appSettingsModal.value?.showInstanceDefaults(tab),
})

watch(incompatibilityWarningModal, (modal) => {
	if (modal) {
		setContentIncompatibilityWarningModal(modal)
	}
})

const authProvider = setupAuthProvider(credentials, async (_redirectPath, flow, options) => {
	if (options?.showModal === false) {
		await signIn(flow)
	} else {
		await requestSignIn(flow)
	}
})

const userPreferences = setupAppUserPreferencesProvider(authProvider, notificationManager)
let userPreferencesSync = Promise.resolve()

watch(
	[userPreferences.preferences, stateInitialized],
	([preferences, initialized]) => {
		if (!preferences || !initialized) return

		userPreferencesSync = userPreferencesSync
			.then(async () => {
				const settings = await getSettings()
				const selectedTheme = preferences.appearance.auto ? 'system' : preferences.appearance.theme
				const userId = credentials.value?.user_id ?? credentials.value?.user?.id
				if (userId) {
					rememberAccountAppearance(userId, preferences.appearance)
				}
				if (appTheme.syncAcrossDevices && isDarkTheme(preferences.appearance.theme)) {
					appTheme.preferredDark = preferences.appearance.theme
				}
				const locale = preferences.localization.locale
				const behavior = preferences.behavior
				let settingsChanged = false

				if (appTheme.syncAcrossDevices && appTheme.preferred !== selectedTheme) {
					appTheme.preferred = selectedTheme
				}
				if (i18n.global.locale.value !== locale) {
					await setLocale(locale)
				}

				if (appTheme.syncAcrossDevices && settings.theme !== selectedTheme) {
					settings.theme = selectedTheme
					settingsChanged = true
				}
				if (settings.locale !== locale) {
					settings.locale = locale
					settingsChanged = true
				}

				if (behavior && appSettings.syncBehaviorAcrossDevices) {
					const behaviorSettings = {
						refocus_on_game_close: 'refocusOnGameClose',
						compact_instance_cards: 'compactInstanceCards',
						show_play_time: 'showPlayTime',
						warn_on_unknown_modpacks: 'warnOnUnknownModpacks',
						skip_non_essential_warnings: 'skipNonEssentialWarnings',
					}
					for (const [key, stateKey] of Object.entries(behaviorSettings)) {
						const value = behavior[key] ?? settings[key]
						appSettings[stateKey] = value
						if (settings[key] !== value) {
							settings[key] = value
							settingsChanged = true
						}
					}

					appSettings.toggleSidebar = behavior.hide_right_sidebar
					appSettings.hideNametagSkinsPage = behavior.hide_nametag

					if (settings.hide_on_process_start !== behavior.minimize_app) {
						settings.hide_on_process_start = behavior.minimize_app
						settingsChanged = true
					}
					if (settings.toggle_sidebar !== behavior.hide_right_sidebar) {
						settings.toggle_sidebar = behavior.hide_right_sidebar
						settingsChanged = true
					}
					if (settings.hide_nametag_skins_page !== behavior.hide_nametag) {
						settings.hide_nametag_skins_page = behavior.hide_nametag
						settingsChanged = true
					}
				}

				if (behavior && appSettings.syncFeaturesAcrossDevices) {
					const featureSettings = {
						show_jump_in: 'showJumpIn',
						show_files_tab_in_instances: 'showFilesTabInInstances',
						show_worlds_tab_in_instances: 'showWorldsTabInInstances',
						show_screenshots_tab_in_instances: 'showScreenshotsTabInInstances',
						show_skin_selector_in_sidebar: 'showSkinSelectorInSidebar',
					}
					for (const [key, stateKey] of Object.entries(featureSettings)) {
						const value = behavior[key] ?? settings[key]
						appSettings[stateKey] = value
						if (settings[key] !== value) {
							settings[key] = value
							settingsChanged = true
						}
					}
					if (typeof behavior.quick_instance_count === 'number') {
						quickInstances.setLimit(behavior.quick_instance_count)
					}

					const showAllScreenshots = behavior.show_all_screenshots
					if (typeof showAllScreenshots === 'boolean') {
						const globalSyncedOptions =
							globalSyncedOptionsQuery.data.value ??
							(await queryClient.fetchQuery(globalSyncedOptionsQueryOptions()))
						if (globalSyncedOptions.screenshots !== showAllScreenshots) {
							const updatedGlobalSyncedOptions = await set_global_synced_option(
								'screenshots',
								showAllScreenshots,
							)
							queryClient.setQueryData(syncedOptionsKeys.global, updatedGlobalSyncedOptions)
							await queryClient.invalidateQueries({ queryKey: screenshotKeys.all })
						}
					}
				}

				if (settingsChanged) {
					await setSettings(settings)
					queryClient.setQueryData(appSettingsKeys.all, settings)
				}
			})
			.catch(handleError)
	},
	{ immediate: true },
)

async function validateSession(sessionToken) {
	try {
		const response = await tauriFetch(`${config.labrinthBaseUrl}/v2/user`, {
			method: 'GET',
			headers: { Authorization: sessionToken },
		})
		if (response.status === 401) return false
		return true
	} catch {
		return true
	}
}

async function fetchCredentials() {
	const hadSession = !!credentials.value?.session
	const refreshId = ++credentialsRefreshId
	credentials.value = undefined

	const creds = await traceStartupStep('Read stored credentials', getCreds).catch(handleError)
	if (refreshId !== credentialsRefreshId) return
	if (!creds && hadSession) clearLiveNotifications()

	if (creds && creds.user_id) {
		if (
			creds.session &&
			!(await traceStartupStep('Validate stored session', () => validateSession(creds.session)))
		) {
			if (refreshId !== credentialsRefreshId) return

			clearLiveNotifications()
			await removeUser(creds.user_id).catch(handleError)
			if (refreshId !== credentialsRefreshId) return

			credentials.value = null
			liveNotificationsEnabled = false
			await fetchStoredModrinthAccounts()
			return
		}
		creds.user = await traceStartupStep('Fetch signed-in user', () =>
			get_user(creds.user_id, 'bypass'),
		).catch(handleError)
		if (refreshId !== credentialsRefreshId) return
	}
	credentials.value = creds ?? null
	liveNotificationsEnabled = !!creds?.session
	await traceStartupStep('Load stored account profiles', fetchStoredModrinthAccounts)
}

async function signIn(flow = 'sign-in', addAccount = false) {
	try {
		await login(flow, addAccount)
		await fetchCredentials()
	} catch (error) {
		if (error?.code === 'login_canceled') {
			// user closed the login window
		} else {
			handleError(error)
		}
	}
}

async function requestSignIn(flow = 'sign-in', addAccount = false) {
	await modrinthLoginModal.value?.showSigningIn(flow, addAccount)
}

async function requestModrinthAuth(flow = 'sign-in', addAccount = false) {
	await signIn(flow, addAccount)
	return !!credentials.value?.session
}

async function logOut() {
	if (!credentials.value?.user) return
	await completeAccountSwitch(() => logout())
}

async function fetchStoredModrinthAccounts() {
	const all = (await getAllCreds().catch(handleError)) ?? []
	const ids = all.map((account) => account.user_id)
	const users = ids.length ? ((await get_user_many(ids).catch(handleError)) ?? []) : []
	const usersById = new Map(users.map((user) => [user.id, user]))

	storedModrinthAccounts.value = all.map((account) => ({
		...account,
		user: usersById.get(account.user_id) ?? {
			id: account.user_id,
			username: account.user_id,
			avatar_url: null,
			role: 'developer',
		},
	}))
}

const accountSwitcherAccounts = computed(() => {
	const currentId = credentials.value?.session ? credentials.value.user_id : null

	return storedModrinthAccounts.value.map((account) => ({
		...account,
		optionId: `account-${account.user_id}`,
		current: account.user_id === currentId,
	}))
})

const profileButtonTooltip = computed(() => {
	if (credentials.value === undefined) return formatMessage(messages.loadingProfile)
	if (credentials.value?.user) return formatMessage(messages.modrinthAccount)
	return formatMessage(messages.signInToModrinthAccount)
})

const accountSwitcherOptions = computed(() => [
	...accountSwitcherAccounts.value.map((account) => ({
		id: account.optionId,
		label: account.user.username,
		selected: account.current,
		action: () => switchModrinthAccount(account),
		trailingAction: {
			label: formatMessage(messages.removeAccount),
			icon: XIcon,
			color: 'red',
			action: () => forgetModrinthAccount(account.user_id),
		},
	})),
	{
		type: 'divider',
	},
	{
		id: 'add-account',
		label: formatMessage(messages.addAccount),
		icon: PlusIcon,
		action: () => requestSignIn('sign-in', true),
	},
])

const isSwitchingAccount = ref(false)

async function persistAppearanceTheme(appearance) {
	const selectedTheme = appearance.auto ? 'system' : appearance.theme
	appTheme.applyAccountAppearance(appearance)
	const settings = await getSettings()
	if (settings.theme !== selectedTheme) {
		settings.theme = selectedTheme
		await setSettings(settings)
	}
}

async function completeAccountSwitch(task) {
	isSwitchingAccount.value = true
	await nextTick()
	try {
		await task()
		window.location.reload()
	} catch (error) {
		isSwitchingAccount.value = false
		handleError(error)
	}
}

async function switchModrinthAccount(account) {
	if (account.current) return

	const cached = getAccountAppearance(account.user_id)
	if (cached) await persistAppearanceTheme(cached)
	await nextTick()

	await completeAccountSwitch(async () => {
		await setActive(account.user_id)
		if (cached) return

		try {
			const preferences = await get_user_preferences(account.user_id)
			rememberAccountAppearance(account.user_id, preferences.appearance)
			await persistAppearanceTheme(preferences.appearance)
			await nextTick()
		} catch {
			// no saved appearance for this account
		}
	})
}

async function forgetModrinthAccount(userId) {
	const isCurrent = credentials.value?.user_id === userId && !!credentials.value?.session
	if (isCurrent) {
		await completeAccountSwitch(() => removeUser(userId))
		return
	}

	await removeUser(userId).catch(handleError)
	await fetchStoredModrinthAccounts()
}

const modrinthAccountMenuOptions = computed(() => [
	{
		id: 'view-profile',
		label: formatMessage(messages.viewProfile),
		icon: UserIcon,
		action: () => router.push(`/user/${encodeURIComponent(credentials.value.user.username)}`),
	},
	{
		id: 'plus',
		label: formatMessage(messages.upgradeToModrinthPlus),
		icon: ArrowBigUpDashIcon,
		type: 'link',
		href: 'https://modrinth.plus?app',
		target: '_blank',
		tone: 'purple',
		shown: !hasPlus.value,
	},
	{
		id: 'add-friend',
		label: formatMessage(messages.addFriend),
		icon: UserPlusIcon,
		action: () => sidebar.value?.showAddFriendModal(),
	},
	{
		id: 'flags',
		label: formatMessage(commonSettingsMessages.featureFlags),
		icon: ToggleRightIcon,
		shown: appSettings.devMode,
		action: settingsActions.openFeatureFlags,
	},
	{
		type: 'divider',
	},
	{
		id: 'switch-account',
		label: formatMessage(messages.switchAccount),
		icon: ArrowLeftRightIcon,
		type: 'submenu',
		options: accountSwitcherOptions.value,
	},
	{
		id: 'sign-out',
		label: formatMessage(commonMessages.signOutButton),
		icon: LogOutIcon,
		tone: 'red',
		action: () => logOut(),
	},
])

async function fetchIntercomToken() {
	const creds = await getCreds()
	if (!creds?.session) {
		throw new Error('Not authenticated')
	}

	const params = new URLSearchParams()
	const rawServerId = route.params.id
	const serverId = Array.isArray(rawServerId) ? rawServerId[0] : rawServerId
	if (route.path.startsWith('/hosting/manage/') && typeof serverId === 'string') {
		params.set('server_id', serverId)
	}
	const query = params.size > 0 ? `?${params.toString()}` : ''

	const response = await tauriFetch(`${config.siteUrl}/api/intercom/messenger-jwt${query}`, {
		method: 'GET',
		headers: {
			Authorization: `Bearer ${creds.session}`,
		},
	})
	if (!response.ok) {
		throw new Error(`Failed to fetch Intercom token: ${response.status}`)
	}
	return await response.json()
}

watch(
	[stateInitialized, showAd, adConsentAvailable],
	async ([ready, showAds, canManageConsent]) => {
		if (!ready) return

		if (showAds) {
			await init_ads_window(true)
			return
		}

		await hide_ads_window(true)
		if (canManageConsent) {
			await init_ads_window()
		}
	},
	{ immediate: true },
)

onMounted(() => {
	invoke('show_window')

	error.setErrorModal(errorModal.value)
	error.setMinecraftAuthErrorModal(minecraftAuthErrorModal.value)
	error.setMinecraftRequiredModal(minecraftRequiredModal.value)

	setContentIncompatibilityWarningModal(incompatibilityWarningModal.value)
	setContentInstallModal(modInstallModal.value)
	setContentInstallModpackAlreadyInstalledModal(contentInstallModpackAlreadyInstalledModal.value)
	setModpackAlreadyInstalledModal(modpackAlreadyInstalledModal.value)
	setServerAddServerToInstanceModal(addServerToInstanceModal.value)
	setServerInstallToPlayModal(installToPlayModal.value)
	setServerUpdateToPlayModal(updateToPlayModal.value)

	window.addEventListener('keydown', handleGlobalKeydown)
	startNetworkMonitor()
})

onUnmounted(() => {
	window.removeEventListener('keydown', handleGlobalKeydown)
	stopNetworkMonitor()
})

let networkMonitorInterval = null
function startNetworkMonitor() {
	if (networkMonitorInterval) clearInterval(networkMonitorInterval)
	networkMonitorInterval = setInterval(() => {
		void checkNetworkHealthTick()
	}, 15_000)
}
function stopNetworkMonitor() {
	if (networkMonitorInterval) {
		clearInterval(networkMonitorInterval)
		networkMonitorInterval = null
	}
}

const effectiveMaintenanceLock = computed(
	() => (maintenanceMode.value && !isDeveloper.value) || (isDeveloper.value && debugPreview.value.forceMaintenance),
)
const effectiveStartupOfflineLock = computed(
	() =>
		(startupBlockedOffline.value && !isDeveloper.value) ||
		(isDeveloper.value && debugPreview.value.forceNetworkState === 'startup_offline'),
)
const effectiveLostConnectionLock = computed(
	() =>
		(connectionLostFatal.value && !isDeveloper.value) ||
		(isDeveloper.value && debugPreview.value.forceNetworkState === 'lost_connection'),
)

async function restartLauncher() {
	try {
		await invoke('restart_app')
	} catch {
		window.location.reload()
	}
}

async function applyDebugViewport(preset) {
	debugPreview.value.viewportPreset = preset
	try {
		const win = getCurrentWindow()
		const { LogicalSize } = await import('@tauri-apps/api/dpi')
		if (preset === 'compact') {
			await win.setSize(new LogicalSize(940, 620))
		} else if (preset === 'standard') {
			await win.setSize(new LogicalSize(1220, 760))
		} else if (preset === 'wide') {
			await win.setSize(new LogicalSize(1480, 880))
		}
	} catch (e) {
		console.warn('Could not resize window for debug viewport:', e)
	}
}

const developerMenuOpen = ref(false)
function handleGlobalKeydown(e) {
	if (e.shiftKey && (e.key === 'F10' || e.code === 'F10')) {
		e.preventDefault()
		developerMenuOpen.value = !developerMenuOpen.value
	} else if (e.shiftKey && (e.key === 'F9' || e.code === 'F9')) {
		if (isDeveloper.value) {
			e.preventDefault()
			debugPreview.value.open = !debugPreview.value.open
		}
	} else if (e.key === 'Escape') {
		if (debugPreview.value.open) {
			debugPreview.value.open = false
		} else if (developerMenuOpen.value) {
			developerMenuOpen.value = false
		} else {
			deselectModpack()
		}
	}
}

const shortcutNotice = ref(null)

function dismissShortcutNotice() {
	shortcutNotice.value = null
}

async function handleOpenLauncherToUpdate() {
	if (!shortcutNotice.value) return
	const serverId = shortcutNotice.value.serverId
	shortcutNotice.value = null
	if (serverId) {
		selectedId.value = serverId
		currentView.value = 'home'
		if (router.currentRoute.value.path !== '/') {
			await router.push('/').catch(() => {})
		}
	}
	try {
		await invoke('show_window')
	} catch {}
}

const sidebar = ref(null)
provideMinecraftAccounts(computed(() => sidebar.value?.minecraftAccounts ?? null))

useAppEvent('command', handleCommand, appEvents)
useAppEvent('notification', handleLiveNotification, appEvents)

async function markLiveNotificationRead(notification) {
	try {
		await tauriApiClient.labrinth.notifications_v2.markAsRead(notification.id)
	} catch (error) {
		if (error instanceof ModrinthApiError && error.statusCode === 404) {
			console.warn(`notification ${notification.id} could not be marked as read`, error)
			return
		}
		throw error
	}
}

async function respondToServerInvite(notification, action) {
	const serverId = notification.body?.server_id
	if (typeof serverId !== 'string') {
		throw new Error('Missing server ID for invite notification.')
	}

	await tauriApiClient.request(`/servers/${serverId}/invites/${action}`, {
		api: 'archon',
		version: 1,
		method: 'POST',
	})
	await markLiveNotificationRead(notification)

	return serverId
}

async function acceptServerInviteNotification(notification) {
	try {
		const serverId = await respondToServerInvite(notification, 'accept')
		await router.push(`/hosting/manage/${encodeURIComponent(serverId)}`)
		queryClient.invalidateQueries({ queryKey: ['servers'] })
	} catch (error) {
		handleError(error)
	}
}

async function declineServerInviteNotification(notification) {
	try {
		await respondToServerInvite(notification, 'decline')
	} catch (error) {
		handleError(error)
	}
}

function openServerInviteInviterProfile(inviterName) {
	if (!inviterName) return
	void router.push(`/user/${encodeURIComponent(inviterName)}`)
}

async function handleLiveNotification(notification) {
	if (!liveNotificationsEnabled || !notification?.body || notification.read) return
	if (await sharedInstanceInviteHandler.value?.handleNotification(notification)) return

	if (notification.body.type === 'server_invite') {
		if (displayedServerInviteNotifications.has(notification.id)) return

		const generation = liveNotificationGeneration
		displayedServerInviteNotifications.add(notification.id)

		const serverName =
			typeof notification.body.server_name === 'string' ? notification.body.server_name : 'a server'
		const inviterId = notification.body.invited_by
		const invitedBy =
			typeof inviterId === 'string' ? await get_user(inviterId, 'bypass').catch(() => null) : null
		if (generation !== liveNotificationGeneration) return

		const popupNotification = addPopupNotification({
			contentType: 'toast',
			title: serverName,
			type: 'server-invite',
			actorName: invitedBy?.username ?? null,
			actorAvatarUrl: invitedBy?.avatar_url ?? null,
			entityName: serverName,
			autoCloseMs: null,
			onAccept: () => acceptServerInviteNotification(notification),
			onDecline: () => declineServerInviteNotification(notification),
			onOpenActor: () => openServerInviteInviterProfile(invitedBy?.username ?? null),
		})
		serverInvitePopupNotificationIds.add(popupNotification.id)
	}
}

function clearLiveNotifications() {
	liveNotificationGeneration++
	liveNotificationsEnabled = false
	for (const id of serverInvitePopupNotificationIds) {
		popupNotificationManager.removeNotification(id)
	}
	displayedServerInviteNotifications.clear()
	serverInvitePopupNotificationIds.clear()
	sharedInstanceInviteHandler.value?.clearNotifications()
}

async function handleCommand(e) {
	if (!e) return

	if (e.event === 'RunMRPack') {
		// RunMRPack should directly install a local mrpack given a path
		if (e.path.endsWith('.mrpack')) {
			const location = { type: 'fromFile', path: e.path }
			const preview = await install_get_modpack_preview(location).catch(handleError)
			if (preview?.unknownFile || preview?.externalFilesInModpack.length > 0) {
				const splitPath = e.path.split(/[\\/]/)
				const fileName = splitPath ? splitPath[splitPath.length - 1] : e.path
				unknownPackWarningModal.value?.show(
					() => install_create_modpack_instance(location).then(() => undefined),
					fileName,
					preview.externalFilesInModpack,
				)
			} else {
				await install_create_modpack_instance(location).catch(handleError)
			}
			trackEvent('InstanceCreate', {
				source: 'CreationModalFileDrop',
			})
		}
	} else if (e.event === 'LaunchInstance') {
		await Promise.all([
			fetchVersionsData().catch(() => {}),
			initDr3shBackendState().catch(() => {}),
		])

		const rawTargetId = String(e.id || '').trim()
		const rawTargetLower = rawTargetId.toLowerCase()
		let instance = rawTargetId ? await getInstance(rawTargetId).catch(() => null) : null
		const allInstances = await listInstances().catch(() => [])
		if (!instance && allInstances.length > 0) {
			instance =
				allInstances.find(
					(inst) =>
						String(inst.id || '').toLowerCase() === rawTargetLower ||
						String(inst.path || '').toLowerCase() === rawTargetLower ||
						String(inst.name || '').toLowerCase() === rawTargetLower,
				) || null
		}

		// Find matching Dr3sh server for this shortcut (works with instance ID, server ID, or title)
		const server =
			servers.value.find((s) => {
				if (String(s.id || '').toLowerCase() === rawTargetLower) return true
				if (String(getInstalledInstanceId(s.id) || '').toLowerCase() === rawTargetLower) return true
				if (String(s.title || '').toLowerCase() === rawTargetLower) return true
				if (instance && Boolean(findMatchingInstance(s, [instance]))) return true
				return false
			}) || null

		if (!instance && server) {
			instance = findMatchingInstance(server, allInstances)
		}

		if (server) {
			selectedId.value = server.id
			currentView.value = 'home'
			if (router.currentRoute.value.path !== '/') {
				await router.push('/').catch(() => {})
			}
		}

		// Check maintenance mode
		if (maintenanceMode.value && !isDeveloper.value) {
			shortcutNotice.value = {
				type: 'maintenance',
				title: 'Launcher en Mantenimiento',
				message:
					maintenanceMessage.value ||
					'Estamos realizando tareas de mantenimiento en nuestros servidores. El acceso se restablecerá automáticamente cuando finalice.',
				instanceId: instance?.id || e.id,
				serverId: server?.id,
			}
			try {
				await invoke('show_window')
			} catch {}
			return
		}

		if (server) {
			// Check if server is disabled
			if (server.disabled && !isDeveloper.value) {
				shortcutNotice.value = {
					type: 'disabled',
					title: 'Modpack no disponible',
					message:
						server.disabled_message ||
						`El modpack "${server.title}" se encuentra temporalmente inhabilitado.`,
					instanceId: instance?.id || e.id,
					serverId: server.id,
				}
				try {
					await invoke('show_window')
				} catch {}
				return
			}

			// Check countdown
			if (server.release_date && !isDeveloper.value) {
				const targetTime = new Date(server.release_date).getTime()
				if (!isNaN(targetTime) && targetTime > Date.now()) {
					const diffMs = targetTime - Date.now()
					const totalSec = Math.floor(diffMs / 1000)
					const days = Math.floor(totalSec / 86400)
					const hours = Math.floor((totalSec % 86400) / 3600)
					const mins = Math.floor((totalSec % 3600) / 60)
					const secs = totalSec % 60
					const formattedTime =
						days > 0
							? `${days}d ${hours}h ${mins}m ${secs}s`
							: `${hours}h ${mins}m ${secs}s`

					shortcutNotice.value = {
						type: 'countdown',
						title: 'Estreno Próximo',
						message: `El modpack "${server.title}" aún no está disponible. Faltan ${formattedTime} para el estreno (Horario de Venezuela).`,
						instanceId: instance?.id || e.id,
						serverId: server.id,
					}
					try {
						await invoke('show_window')
					} catch {}
					return
				}
			}

			// Check if there is an update available
			const installedVer = getInstalledVersion(server.id)
			const latestVer = server.versions?.[0]?.version
			const cleanInstalled = installedVer ? installedVer.replace(/^v/i, '').trim() : ''
			const cleanLatest = latestVer ? String(latestVer).replace(/^v/i, '').trim() : ''
			if (cleanInstalled && cleanLatest && cleanInstalled !== cleanLatest) {
				shortcutNotice.value = {
					type: 'update',
					title: 'Actualización disponible',
					message: `Hay una nueva versión disponible para "${server.title}" (v${cleanInstalled} → v${cleanLatest}). ¿Deseas abrir el launcher para actualizar o cancelar?`,
					instanceId: instance?.id || e.id,
					serverId: server.id,
					installedVersion: installedVer,
					latestVersion: latestVer,
				}
				try {
					await invoke('show_window')
				} catch {}
				return
			}
		}

		if (!instance || instance.quarantined) {
			if (server) {
				shortcutNotice.value = {
					type: 'repair',
					title: 'Instancia no encontrada',
					message: `La instancia de "${server.title}" no está lista o fue eliminada del disco. Abre el launcher para descargarla o repararla.`,
					instanceId: e.id,
					serverId: server.id,
				}
				try {
					await invoke('show_window')
				} catch {}
			}
			return
		}

		if (server) {
			try {
				let fallbackPath = getInstalledFolder(server.id)
				if (!fallbackPath && instance.id) {
					fallbackPath = await get_full_path(instance.id).catch(() => null)
				}
				const health = await invoke('dr3sh_check_instance_health', {
					serverId: server.id,
					fallbackInstancePath: fallbackPath || null,
				})
				if (health?.needs_repair) {
					shortcutNotice.value = {
						type: 'repair',
						title: 'La instancia requiere reparación',
						message:
							health.reason ||
							`Faltan archivos vitales en "${server.title}". Abre el launcher y pulsa "Reparar instancia".`,
						instanceId: instance.id,
						serverId: server.id,
					}
					try {
						await invoke('show_window')
					} catch {}
					return
				}
			} catch {}

			if (!activeAccount.value) {
				await refreshAccount().catch(() => {})
			}
			await invoke('dr3sh_prepare_launch', {
				serverId: server.id,
				isOffline: !activeAccount.value?.isMicrosoft,
				offlineUsername: activeAccount.value?.name || 'Player',
				bypassGate: Boolean(isDeveloper.value),
			})
			const savedFolder = getInstalledFolder(server.id)
			if (savedFolder) {
				const instDir = await get_full_path(instance.id).catch(() => null)
				if (instDir && instDir.toLowerCase() !== savedFolder.toLowerCase()) {
					await invoke('dr3sh_link_existing_instance', {
						instanceId: instance.id,
						instanceDir: instDir,
						existingFolder: savedFolder,
					}).catch(() => {})
				}
			}
		}

		if (e.server) {
			await start_join_server(instance.id, e.server).catch(handleError)
		} else if (e.singleplayer_world) {
			await start_join_singleplayer_world(instance.id, e.singleplayer_world).catch(handleError)
		} else {
			await run(instance.id).catch(handleError)
		}
	} else if (e.event === 'InstallSharedInstanceInvite') {
		await sharedInstanceInviteHandler.value?.installFromInviteId(e.invite_id)
	} else if (e.event === 'InstallServer') {
		await router.push(`/project/${e.id}`)
		await playServerProject(e.id).catch(handleError)
	} else if (e.event === 'InstallVersion') {
		const version = await get_version(e.id, 'must_revalidate').catch(handleError)
		if (version) {
			await contentInstall
				.install(version.project_id, version.id, null, 'URLConfirmModal', undefined, undefined, {
					showProjectInfo: true,
				})
				.catch(handleError)
		}
	} else {
		await contentInstall
			.install(e.id, null, null, 'URLConfirmModal', undefined, undefined, { showProjectInfo: true })
			.catch(handleError)
	}
}

const appUpdateDownload = {
	progress: appUpdateState.progress,
	version: ref(),
}
let unlistenUpdateDownload

const { availableUpdate } = appUpdateState
let launcherUpdateCheckTimer = null
const launcherUpdateInfo = ref(null)
const dismissedLauncherUpdateVersion = ref(null)

function clearDelayedUpdatePopup() {
	if (launcherUpdateCheckTimer !== null) {
		clearTimeout(launcherUpdateCheckTimer)
		launcherUpdateCheckTimer = null
	}
}

function dismissLauncherUpdateModal() {
	if (launcherUpdateInfo.value?.latestVersion) {
		dismissedLauncherUpdateVersion.value = launcherUpdateInfo.value.latestVersion
	}
	launcherUpdateInfo.value = null
}

async function openLauncherUpdateDownload(urlOverride) {
	const info = launcherUpdateInfo.value
	const target =
		urlOverride ||
		info?.downloadUrl ||
		info?.releasePageUrl ||
		'https://github.com/Dr3sh/dr3shapp/releases/latest'
	try {
		await openUrl(target)
	} catch {
		if (info?.releasePageUrl && target !== info.releasePageUrl) {
			try {
				await openUrl(info.releasePageUrl)
			} catch {}
		}
	}
}

async function checkUpdates() {
	if (launcherUpdateCheckTimer !== null) {
		clearTimeout(launcherUpdateCheckTimer)
		launcherUpdateCheckTimer = null
	}

	async function performCheck() {
		if (typeof navigator !== 'undefined' && !navigator.onLine) {
			return
		}
		try {
			const res = await invoke('dr3sh_check_launcher_update')
			if (!res || !res.updateAvailable) {
				return
			}
			availableUpdate.value = {
				rid: 0,
				version: res.latestVersion,
				currentVersion: res.currentVersion,
			}
			markAppUpdateActionable(res.latestVersion, 'available')
			if (dismissedLauncherUpdateVersion.value === res.latestVersion) {
				return
			}
			launcherUpdateInfo.value = res
		} catch (e) {
			console.warn('Update check failed (offline or unreachable):', e)
		}
	}

	await performCheck()
	launcherUpdateCheckTimer = setTimeout(
		() => {
			checkUpdates()
		},
		10 /* min */ * 60 /* sec */ * 1000 /* ms */,
	)
}

async function downloadAvailableUpdate() {
	return openLauncherUpdateDownload()
}

async function installUpdate() {
	return openLauncherUpdateDownload()
}

setAppUpdateActions({
	download: downloadAvailableUpdate,
	install: installUpdate,
	changelog: () =>
		openUrl(launcherUpdateInfo.value?.releasePageUrl || 'https://github.com/Dr3sh/dr3shapp/releases'),
})

async function openModrinthProjectLinkInApp(parsed) {
	const { slug, pathSuffix, url } = parsed
	const loadToken = loading.begin()
	try {
		const { id } = await tauriApiClient.labrinth.projects_v2.check(slug)
		const query = mergeUrlQuery(route.query, url)
		await router.push({
			path: `/project/${id}${pathSuffix}`,
			query,
			hash: url.hash || undefined,
		})
	} catch (err) {
		if (err instanceof ModrinthApiError && err.statusCode === 404) {
			openUrl(url.href)
		} else {
			handleError(err)
		}
	} finally {
		loading.end(loadToken)
	}
}

function handleClick(e) {
	let target = e.target
	while (target != null) {
		if (target.matches('a')) {
			if (
				target.href &&
				['http://', 'https://', 'mailto:', 'tel:'].some((v) => target.href.startsWith(v)) &&
				!target.classList.contains('router-link-active') &&
				!target.href.startsWith('http://localhost') &&
				!target.href.startsWith('https://tauri.localhost') &&
				!target.href.startsWith('http://tauri.localhost')
			) {
				const userPath = parse_modrinth_user_link(target.href)
				const parsed = parseModrinthLink(target.href)
				if (userPath) {
					void router.push(userPath)
				} else if (target.target !== '_blank' && parsed) {
					void openModrinthProjectLinkInApp(parsed)
				} else {
					openUrl(target.href)
				}
			}
			e.preventDefault()
			break
		}
		target = target.parentElement
	}
}

function handleAuxClick(e) {
	// disables middle click -> new tab
	if (e.button === 1) {
		e.preventDefault()
		// instead do a left click
		const event = new MouseEvent('click', {
			view: window,
			bubbles: true,
			cancelable: true,
		})
		e.target.dispatchEvent(event)
	}
}

function handleContextMenu(event) {
	const target = event.target
	if (target instanceof Element) {
		if (target.closest('img, textarea, [contenteditable="true"]')) return
		const input = target.closest('input')
		if (
			input &&
			!['button', 'checkbox', 'radio', 'submit', 'reset', 'file', 'range'].includes(input.type)
		) {
			return
		}
	}

	const selection = window.getSelection()
	if (
		target instanceof Node &&
		selection &&
		!selection.isCollapsed &&
		selection.containsNode(target, true)
	) {
		return
	}

	event.preventDefault()
}

provideAppUpdateDownloadProgress(appUpdateDownload)
</script>

<template>
	<TooltipDirective />
	<SplashScreen v-if="!stateFailed" ref="splashScreen" data-tauri-drag-region />
	<div id="teleports"></div>
	<AccountSwitchOverlay :show="isSwitchingAccount" />
	<div
		v-if="stateInitialized"
		class="launcher"
		:class="{ 'disable-advanced-rendering': !appTheme.advancedRendering }"
	>
		<Transition name="fade">
			<div
				v-if="restarting"
				data-tauri-drag-region
				class="inset-0 fixed bg-black/80 backdrop-blur z-[200] flex items-center justify-center"
			>
				<span
					data-tauri-drag-region
					class="flex items-center gap-4 text-contrast font-semibold text-xl select-none cursor-default"
				>
					<RefreshCwIcon data-tauri-drag-region class="animate-spin w-6 h-6" />
					{{ formatMessage(messages.restarting) }}
				</span>
			</div>
		</Transition>

		<div v-if="effectiveMaintenanceLock" class="maintenance-lock-view">
			<div class="maintenance-bg-glow" aria-hidden="true" />
			<header data-tauri-drag-region class="maintenance-lock-topbar">
				<div class="brand-lockup-simple" data-tauri-drag-region>
					<img src="/logo.png" alt="Dr3sh Launcher" class="w-6 h-6 rounded-md" />
					<span class="font-bold text-sm text-white/90">Dr3sh Launcher</span>
				</div>
				<div class="window-controls" aria-label="Controles de ventana" data-tauri-drag-region-exclude>
					<button class="window-control" title="Minimizar" aria-label="Minimizar" @click="getCurrentWindow().minimize()">
						<span class="minimize-glyph" />
					</button>
					<button class="window-control close" title="Cerrar" aria-label="Cerrar" @click="handleClose">
						<span class="close-glyph" />
					</button>
				</div>
			</header>
			<div class="maintenance-lock-card">
				<div class="maintenance-icon-bubble">
					<svg viewBox="0 0 24 24" width="38" height="38" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
						<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
					</svg>
				</div>
				<span class="maintenance-pill">
					<span class="maintenance-pulse-dot" />
					Mantenimiento Activo
				</span>
				<h2>Estamos en mantenimiento!</h2>
				<p v-if="maintenanceMessage" class="maintenance-msg">
					{{ maintenanceMessage }}
				</p>
				<div class="maintenance-actions-row">
					<button
						type="button"
						class="maintenance-retry-btn"
						:disabled="retryingCatalog"
						@click="fetchVersionsData(true)"
					>
						{{ retryingCatalog ? 'Comprobando...' : 'Verificar estado ahora' }}
					</button>
					<button
						v-if="isDeveloper && debugPreview.forceMaintenance"
						type="button"
						class="maintenance-debug-exit-btn"
						@click="debugPreview.forceMaintenance = false"
					>
						Salir de vista Debug
					</button>
				</div>
				<div class="maintenance-pulse-indicator">
					<span>Comprobación automática cada pocos segundos</span>
				</div>
			</div>
		</div>

		<div v-else-if="effectiveStartupOfflineLock" class="maintenance-lock-view is-offline-lock">
			<div class="maintenance-bg-glow is-red" aria-hidden="true" />
			<header data-tauri-drag-region class="maintenance-lock-topbar">
				<div class="brand-lockup-simple" data-tauri-drag-region>
					<img src="/logo.png" alt="Dr3sh Launcher" class="w-6 h-6 rounded-md" />
					<span class="font-bold text-sm text-white/90">Dr3sh Launcher</span>
				</div>
				<div class="window-controls" aria-label="Controles de ventana" data-tauri-drag-region-exclude>
					<button class="window-control" title="Minimizar" aria-label="Minimizar" @click="getCurrentWindow().minimize()">
						<span class="minimize-glyph" />
					</button>
					<button class="window-control close" title="Cerrar" aria-label="Cerrar" @click="handleClose">
						<span class="close-glyph" />
					</button>
				</div>
			</header>
			<div class="maintenance-lock-card is-offline-card">
				<div class="maintenance-icon-bubble is-red">
					<svg viewBox="0 0 24 24" width="36" height="36" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
						<path d="M12 20h.01" />
						<path d="M8.5 16.429a5 5 0 0 1 7 0" />
						<path d="M5 12.859a10 10 0 0 1 5.17-2.69" />
						<path d="M19 12.859a10 10 0 0 0-2.007-1.523" />
						<path d="M2 8.82a15 15 0 0 1 4.177-2.643" />
						<path d="M22 8.82a15 15 0 0 0-11.288-3.764" />
						<path d="m2 2 20 20" />
					</svg>
				</div>
				<span class="maintenance-pill is-red">Sin conexión</span>
				<h2>No se detectó conexión a internet</h2>
				<p class="maintenance-msg">
					{{ catalogError || 'Dr3sh Launcher requiere conexión a internet al iniciar para verificar servidores, cuentas y actualizaciones.' }}
				</p>
				<div class="maintenance-actions-row">
					<button
						type="button"
						class="maintenance-retry-btn"
						:disabled="retryingCatalog"
						@click="fetchVersionsData(true)"
					>
						{{ retryingCatalog ? 'Conectando...' : 'Reintentar conexión' }}
					</button>
					<button
						v-if="isDeveloper && debugPreview.forceNetworkState === 'startup_offline'"
						type="button"
						class="maintenance-debug-exit-btn"
						@click="debugPreview.forceNetworkState = 'normal'"
					>
						Salir de vista Debug
					</button>
				</div>
			</div>
		</div>

		<div v-else-if="effectiveLostConnectionLock" class="maintenance-lock-view is-offline-lock">
			<div class="maintenance-bg-glow is-red" aria-hidden="true" />
			<header data-tauri-drag-region class="maintenance-lock-topbar">
				<div class="brand-lockup-simple" data-tauri-drag-region>
					<img src="/logo.png" alt="Dr3sh Launcher" class="w-6 h-6 rounded-md" />
					<span class="font-bold text-sm text-white/90">Dr3sh Launcher</span>
				</div>
				<div class="window-controls" aria-label="Controles de ventana" data-tauri-drag-region-exclude>
					<button class="window-control" title="Minimizar" aria-label="Minimizar" @click="getCurrentWindow().minimize()">
						<span class="minimize-glyph" />
					</button>
					<button class="window-control close" title="Cerrar" aria-label="Cerrar" @click="handleClose">
						<span class="close-glyph" />
					</button>
				</div>
			</header>
			<div class="maintenance-lock-card is-offline-card">
				<div class="maintenance-icon-bubble is-red">
					<svg viewBox="0 0 24 24" width="36" height="36" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
						<path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
						<line x1="12" y1="9" x2="12" y2="13" />
						<line x1="12" y1="17" x2="12.01" y2="17" />
					</svg>
				</div>
				<span class="maintenance-pill is-red">Conexión perdida</span>
				<h2>Se perdió la conexión a internet</h2>
				<p class="maintenance-msg">
					Se perdió la conexión con los servidores. Reinicia el launcher cuando vuelva tu internet para continuar.
				</p>
				<div class="maintenance-actions-row">
					<button type="button" class="maintenance-retry-btn" @click="restartLauncher">
						Reiniciar launcher
					</button>
					<button
						v-if="isDeveloper && debugPreview.forceNetworkState === 'lost_connection'"
						type="button"
						class="maintenance-debug-exit-btn"
						@click="debugPreview.forceNetworkState = 'normal'"
					>
						Salir de vista Debug
					</button>
				</div>
			</div>
		</div>

		<template v-else>
			<!-- Left Sidebar (Full 100vh) -->
			<Dr3shRail @open-settings="settingsActions.openSettings" />

			<!-- Main Area: Top Bar + Content -->
			<section class="main-area">
				<Dr3shTopBar />
				<div class="content-scroll">
					<SurveyPopup />
					<div
						class="loading-indicator-container h-8 fixed z-50 pointer-events-none"
						:style="{
							top: '50px',
							left: 'var(--rail-width, 268px)',
							width: 'calc(100% - var(--rail-width, 268px))',
						}"
					>
						<LoadingBar position="absolute" />
					</div>
					<Admonition
						v-if="criticalErrorMessage"
						type="critical"
						:header="criticalErrorMessage.header"
						class="m-6 mb-0"
					>
						<div
							class="markdown-body text-primary"
							v-html="renderString(criticalErrorMessage.body ?? '')"
						></div>
					</Admonition>
					<Admonition
						v-if="authUnreachable"
						type="warning"
						:header="formatMessage(messages.authUnreachableHeader)"
						class="m-6 mb-0"
					>
						{{ formatMessage(messages.authUnreachableBody) }}
					</Admonition>
					<HostingUpdateRequired v-if="hostingUpdateRequired" />
					<RouterView v-else v-slot="{ Component }">
						<template v-if="Component">
							<Suspense @pending="onSuspensePending" @resolve="onSuspenseResolve">
								<component :is="Component"></component>
							</Suspense>
						</template>
					</RouterView>
				</div>
			</section>
		</template>
	</div>
	<AppSettingsModal ref="appSettingsModal" />
	<NewUpdateModal ref="newUpdateModal" />
	<Suspense>
		<ModrinthAccountRequiredModal ref="modrinthLoginModal" :request-auth="requestModrinthAuth" />
	</Suspense>
	<CreationFlowModal
		ref="installationModal"
		type="instance"
		show-snapshot-toggle
		:fetch-existing-instance-names="fetchExistingInstanceNames"
		:search-projects="searchProjects"
		:prepare-project-install="prepareCreationProjectInstall"
		:create-project-install="handleCreateAndInstall"
		:get-loader-manifest="getLoaderManifest"
		:randomize-instance-icon="randomizeCreationIcon"
		:customize-instance-icon="customizeCreationIcon"
		@create="handleCreate"
		@browse-modpacks="handleBrowseModpacks"
	/>
	<IconEditorModal
		ref="creationIconEditorModal"
		:config="creationGeneratedIcon?.config"
		@saved="onCreationIconSaved"
	/>
	<UnknownPackWarningModal ref="unknownPackWarningModal" />
	<I18nDebugPanel />
	<aside v-if="isDeveloper && debugPreview.open" class="dr3sh-debug-panel" aria-label="Menú Debug Admin">
		<div class="dr3sh-debug-header">
			<strong>Admin Debug Preview</strong>
			<div class="dr3sh-debug-header-actions">
				<button type="button" class="dr3sh-debug-reset" @click="resetDebugPreview">Reset</button>
				<button type="button" class="dr3sh-debug-close" @click="debugPreview.open = false">×</button>
			</div>
		</div>
		<div class="dr3sh-debug-body">
			<div class="dr3sh-debug-group">
				<span class="dr3sh-debug-label">Estado del botón principal</span>
				<select v-model="debugPreview.forceButtonAction" class="dr3sh-debug-select">
					<option value="auto">Auto (Real)</option>
					<option value="download">Instalar</option>
					<option value="preload">Precargar Modpack</option>
					<option value="preloaded">Precargado · Esperando estreno</option>
					<option value="countdown">Cuenta regresiva</option>
					<option value="activate">Activar y Jugar</option>
					<option value="play">Jugar</option>
					<option value="update">Actualizar</option>
					<option value="repair">Reparar</option>
					<option value="running">Jugando</option>
					<option value="disabled">Inhabilitado</option>
				</select>
			</div>
			<div class="dr3sh-debug-group">
				<span class="dr3sh-debug-label">Pantallas de estado y red</span>
				<label class="dr3sh-debug-check">
					<input v-model="debugPreview.forceMaintenance" type="checkbox" />
					<span>Ver Mantenimiento (como usuario)</span>
				</label>
				<select v-model="debugPreview.forceNetworkState" class="dr3sh-debug-select">
					<option value="normal">Red: Normal</option>
					<option value="startup_offline">Sin internet al iniciar</option>
					<option value="lost_connection">Pérdida &gt;90s (Pedir reinicio)</option>
				</select>
			</div>
			<div class="dr3sh-debug-group">
				<span class="dr3sh-debug-label">Tamaño de ventana</span>
				<div class="dr3sh-debug-size-row">
					<button
						type="button"
						:class="{ active: debugPreview.viewportPreset === 'compact' }"
						@click="applyDebugViewport('compact')"
					>
						940×620
					</button>
					<button
						type="button"
						:class="{ active: debugPreview.viewportPreset === 'standard' }"
						@click="applyDebugViewport('standard')"
					>
						1220×760
					</button>
					<button
						type="button"
						:class="{ active: debugPreview.viewportPreset === 'wide' }"
						@click="applyDebugViewport('wide')"
					>
						1480×880
					</button>
				</div>
			</div>
		</div>
	</aside>
	<NotificationPanel :has-sidebar="sidebarVisible" />
	<PopupNotificationPanel :has-sidebar="sidebarVisible" />
	<ErrorModal ref="errorModal" />
	<MinecraftAuthErrorModal ref="minecraftAuthErrorModal" />
	<MinecraftRequiredModal ref="minecraftRequiredModal" />
	<ContentInstallModal
		ref="modInstallModal"
		:instances="contentInstallInstances"
		:compatible-loaders="contentInstallLoaders"
		:game-versions="contentInstallGameVersions"
		:loading="contentInstallLoading"
		:default-tab="contentInstallDefaultTab"
		:preferred-loader="contentInstallPreferredLoader"
		:preferred-game-version="contentInstallPreferredGameVersion"
		:release-game-versions="contentInstallReleaseGameVersions"
		:project-info="contentInstallProjectInfo"
		:randomize-icon="randomizeCreationIcon"
		:customize-icon="customizeContentInstallIcon"
		@install="handleInstallToInstance"
		@create-and-install="handleCreateAndInstall"
		@navigate="handleContentInstallNavigate"
		@cancel="handleContentInstallCancel"
	/>
	<ModpackAlreadyInstalledModal
		ref="modpackAlreadyInstalledModal"
		@create-anyway="handleModpackDuplicateCreateAnyway"
		@go-to-instance="handleModpackDuplicateGoToInstance"
	/>
	<AddServerToInstanceModal ref="addServerToInstanceModal" />
	<ContentUpdaterModal
		ref="incompatibilityWarningModal"
		mode="incompatibility-warning"
		:versions="contentInstallIncompatibilityWarningVersions"
		:current-game-version="contentInstallIncompatibilityWarningCurrentGameVersion"
		:current-loader="contentInstallIncompatibilityWarningCurrentLoader"
		current-version-id=""
		:is-app="true"
		:project-type="contentInstallIncompatibilityWarningProjectType"
		:project-icon-url="contentInstallIncompatibilityWarningProjectIconUrl"
		:project-name="contentInstallIncompatibilityWarningProjectName"
		:warning="contentInstallIncompatibilityWarningMessage"
		:action-loading="contentInstallIncompatibilityWarningInstalling"
		@update="handleContentInstallIncompatibilityWarningInstall"
		@cancel="handleContentInstallIncompatibilityWarningCancel"
	/>
	<ModpackAlreadyInstalledModal
		ref="contentInstallModpackAlreadyInstalledModal"
		@create-anyway="handleContentInstallModpackDuplicateCreateAnyway"
		@go-to-instance="handleContentInstallModpackDuplicateGoToInstance"
	/>
	<SharedInstanceInviteHandler ref="sharedInstanceInviteHandler" />
	<InstallToPlayModal ref="installToPlayModal" :show-external-warnings="false" />
	<UpdateToPlayModal ref="updateToPlayModal" :show-external-warnings="false" />
	<DeveloperMenu v-model="developerMenuOpen" />
	<!-- Modal de aviso al iniciar desde acceso directo -->
	<div
		v-if="shortcutNotice"
		class="dr3sh-update-backdrop"
		role="dialog"
		aria-modal="true"
		@click="dismissShortcutNotice"
	>
		<div class="dr3sh-update-modal" @click.stop>
			<div class="dr3sh-update-header">
				<div>
					<span class="dr3sh-update-pill" :class="{ 'pill-danger': shortcutNotice.type !== 'update' }">
						{{ shortcutNotice.type === 'update' ? 'Actualización disponible' : 'Aviso de Modpack' }}
					</span>
					<h3 class="dr3sh-update-title">{{ shortcutNotice.title }}</h3>
				</div>
				<button type="button" class="dr3sh-update-close" aria-label="Cerrar" @click="dismissShortcutNotice">
					✕
				</button>
			</div>

			<p class="dr3sh-shortcut-message">{{ shortcutNotice.message }}</p>

			<div class="dr3sh-update-actions">
				<button
					type="button"
					class="dr3sh-update-btn-secondary"
					@click="dismissShortcutNotice"
				>
					{{ shortcutNotice.type === 'update' ? 'Cancelar' : 'Cerrar' }}
				</button>
				<button
					v-if="shortcutNotice.type === 'update'"
					type="button"
					class="dr3sh-update-btn-primary"
					@click="handleOpenLauncherToUpdate"
				>
					Abrir Launcher para actualizar
				</button>
			</div>
		</div>
	</div>
	<div
		v-if="launcherUpdateInfo"
		class="dr3sh-update-backdrop"
		role="dialog"
		aria-modal="true"
		aria-labelledby="dr3sh-update-title"
		@click="dismissLauncherUpdateModal"
	>
		<div class="dr3sh-update-modal" @click.stop>
			<div class="dr3sh-update-header">
				<div>
					<span class="dr3sh-update-pill">Nueva actualización</span>
					<h3 id="dr3sh-update-title" class="dr3sh-update-title">
						{{ launcherUpdateInfo.releaseName || `Dr3sh Launcher v${launcherUpdateInfo.latestVersion}` }}
					</h3>
					<p class="dr3sh-update-versions">
						Instalada: <strong>v{{ launcherUpdateInfo.currentVersion }}</strong> → Disponible:
						<strong class="dr3sh-update-new-ver">v{{ launcherUpdateInfo.latestVersion }}</strong>
					</p>
				</div>
				<button
					type="button"
					class="dr3sh-update-close"
					aria-label="Cerrar"
					@click="dismissLauncherUpdateModal"
				>
					✕
				</button>
			</div>

			<div v-if="launcherUpdateInfo.releaseNotes" class="dr3sh-update-notes">
				{{ launcherUpdateInfo.releaseNotes }}
			</div>

			<p class="dr3sh-update-safe-note">
				Al abrir el instalador, deja marcada la opción <strong>Actualizar</strong> (viene seleccionada por defecto) para conservar todos tus modpacks, mundos y cuentas.
			</p>

			<div class="dr3sh-update-actions">
				<button
					type="button"
					class="dr3sh-update-btn-secondary"
					@click="dismissLauncherUpdateModal"
				>
					Más tarde
				</button>
				<button
					type="button"
					class="dr3sh-update-btn-secondary"
					@click="openLauncherUpdateDownload(launcherUpdateInfo.releasePageUrl)"
				>
					Ver en GitHub
				</button>
				<button
					type="button"
					class="dr3sh-update-btn-primary"
					@click="openLauncherUpdateDownload()"
				>
					Descargar actualización (Recomendado)
				</button>
			</div>
		</div>
	</div>
</template>

<style lang="scss" scoped>
.dr3sh-update-backdrop {
	position: fixed;
	inset: 0;
	z-index: 9999;
	display: flex;
	align-items: center;
	justify-content: center;
	padding: 20px;
	background: rgba(6, 8, 13, 0.72);
	backdrop-filter: blur(8px);
}

.dr3sh-update-modal {
	width: min(520px, 96vw);
	padding: 22px;
	border-radius: 16px;
	background: #141822;
	border: 1px solid rgba(255, 255, 255, 0.12);
	box-shadow: 0 24px 64px rgba(0, 0, 0, 0.55);
	color: #f3f5f8;
	display: flex;
	flex-direction: column;
	gap: 14px;
}

.dr3sh-update-header {
	display: flex;
	align-items: flex-start;
	justify-content: space-between;
	gap: 12px;
}

.dr3sh-update-pill {
	display: inline-block;
	padding: 3px 9px;
	border-radius: 999px;
	font-size: 11px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.04em;
	color: #34d399;
	background: rgba(16, 185, 129, 0.14);
	border: 1px solid rgba(16, 185, 129, 0.35);
	margin-bottom: 8px;

	&.pill-danger {
		color: #f87171;
		background: rgba(239, 68, 68, 0.14);
		border-color: rgba(239, 68, 68, 0.35);
	}
}

.dr3sh-shortcut-message {
	margin: 0;
	padding: 12px 14px;
	border-radius: 10px;
	background: rgba(0, 0, 0, 0.3);
	border: 1px solid rgba(255, 255, 255, 0.08);
	font-size: 13.5px;
	line-height: 1.5;
	color: rgba(243, 245, 248, 0.92);
}

.dr3sh-update-title {
	margin: 0;
	font-size: 18px;
	font-weight: 700;
	color: #ffffff;
}

.dr3sh-update-versions {
	margin: 4px 0 0;
	font-size: 13px;
	color: rgba(243, 245, 248, 0.76);
}

.dr3sh-update-new-ver {
	color: #34d399;
}

.dr3sh-update-close {
	appearance: none;
	border: 1px solid rgba(255, 255, 255, 0.12);
	background: rgba(255, 255, 255, 0.05);
	color: rgba(243, 245, 248, 0.75);
	width: 30px;
	height: 30px;
	border-radius: 8px;
	cursor: pointer;
	font-size: 13px;
	flex-shrink: 0;
}

.dr3sh-update-close:hover {
	background: rgba(255, 255, 255, 0.12);
	color: #ffffff;
}

.dr3sh-update-notes {
	max-height: 180px;
	overflow-y: auto;
	padding: 12px;
	border-radius: 10px;
	background: rgba(0, 0, 0, 0.32);
	border: 1px solid rgba(255, 255, 255, 0.08);
	font-size: 12.5px;
	line-height: 1.5;
	color: rgba(243, 245, 248, 0.88);
	white-space: pre-wrap;
}

.dr3sh-update-safe-note {
	margin: 0;
	padding: 10px 12px;
	border-radius: 10px;
	background: rgba(59, 130, 246, 0.1);
	border: 1px solid rgba(59, 130, 246, 0.28);
	font-size: 12.5px;
	line-height: 1.45;
	color: #dbeafe;
}

.dr3sh-update-actions {
	display: flex;
	flex-wrap: wrap;
	justify-content: flex-end;
	gap: 8px;
	margin-top: 2px;
}

.dr3sh-update-btn-secondary {
	appearance: none;
	border: 1px solid rgba(255, 255, 255, 0.14);
	background: rgba(255, 255, 255, 0.06);
	color: #e5e7eb;
	padding: 9px 14px;
	border-radius: 10px;
	font-size: 13px;
	font-weight: 600;
	cursor: pointer;
}

.dr3sh-update-btn-secondary:hover {
	background: rgba(255, 255, 255, 0.12);
}

.dr3sh-update-btn-primary {
	appearance: none;
	border: none;
	background: linear-gradient(135deg, #10b981, #059669);
	color: #052e16;
	padding: 9px 16px;
	border-radius: 10px;
	font-size: 13px;
	font-weight: 700;
	cursor: pointer;
	box-shadow: 0 6px 20px rgba(16, 185, 129, 0.28);
}

.dr3sh-update-btn-primary:hover {
	filter: brightness(1.08);
}

.launcher {
	display: grid;
	grid-template-columns: var(--rail-width, 268px) minmax(0, 1fr);
	transition: grid-template-columns 250ms cubic-bezier(0.4, 0, 0.2, 1);
	width: 100vw;
	height: 100vh;
	background:
		radial-gradient(circle at 82% 8%, color-mix(in srgb, var(--brand, #1bd96a) 14%, transparent), transparent 42%),
		var(--ink, #0d1017);
	color: var(--text, #f3f5f8);
	overflow: hidden;
}

.main-area {
	display: grid;
	grid-template-rows: 50px minmax(0, 1fr);
	min-width: 0;
	min-height: 0;
	height: 100vh;
	overflow: hidden;
	border: none;
}

.content-scroll {
	height: 100%;
	overflow-y: auto;
	overflow-x: hidden;
	position: relative;
	scrollbar-width: thin;
	scrollbar-color: rgba(255, 255, 255, 0.15) transparent;
}

[data-tauri-drag-region-exclude] {
	-webkit-app-region: no-drag;
}

.disable-advanced-rendering {
	*,
	:deep(*) {
		box-shadow: none !important;
		--tw-drop-shadow:;
	}
}

@media (prefers-reduced-motion: no-preference) {
	.nav-button-animated-enter-active {
		transition: all 0.5s cubic-bezier(0.15, 1.4, 0.64, 0.96);
	}

	.nav-button-animated-leave-active {
		transition: all 0.25s ease;
	}

	.nav-button-animated-enter-active {
		position: relative;
	}

	.nav-button-animated-enter-active::before {
		content: '';
		inset: 0;
		border-radius: 100vw;
		background-color: var(--color-brand-highlight);
		position: absolute;
		animation: pop 0.5s ease-in forwards;
		opacity: 0;
	}

	@keyframes pop {
		0% {
			scale: 0.5;
		}
		50% {
			opacity: 0.5;
		}
		100% {
			scale: 1.5;
		}
	}

	.nav-button-animated-enter-from {
		scale: 0.5;
		translate: -2rem 0;
		opacity: 0;
	}

	.nav-button-animated-leave-to {
		scale: 0.75;
		opacity: 0;
	}

	.fade-enter-active {
		transition: 0.25s ease-in-out;
	}

	.fade-enter-from {
		opacity: 0;
	}
}
</style>
<style>
.os-theme-dark,
.os-theme-light {
	--os-handle-bg: var(--color-scrollbar) !important;
	--os-handle-bg-hover: var(--color-scrollbar) !important;
	--os-handle-bg-active: var(--color-scrollbar) !important;
}

.windows {
	.fake-appbar {
		height: 2.5rem !important;
	}

	.info-card {
		right: 22rem;
	}

	.profile-card {
		right: 8rem;
	}
}
</style>
