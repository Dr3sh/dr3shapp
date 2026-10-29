<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import {
	LogInIcon,
	PlusIcon,
	RadioButtonCheckedIcon,
	RadioButtonIcon,
	SpinnerIcon,
	TrashIcon,
	UserIcon,
} from '@modrinth/assets'
import { Button, IconButton } from '@modrinth/ui'
import {
	add_offline_user,
	get_default_user,
	login as login_flow,
	remove_user,
	set_default_user,
	users,
} from '@/helpers/auth'
import { refreshAccount } from '@/composables/use-dr3sh-launcher'

interface AccountProfile {
	id: string
	name: string
}

interface UserAccount {
	profile: AccountProfile
	refresh_token?: string
}

const accounts = ref<UserAccount[]>([])
const defaultUserId = ref<string | null>(null)
const loading = ref(false)
const loggingIn = ref(false)
const showingOfflineInput = ref(false)
const offlineUsername = ref('')
const errorMsg = ref<string | null>(null)

async function refresh() {
	try {
		loading.value = true
		errorMsg.value = null
		const [allUsers, activeId] = await Promise.all([users(), get_default_user()])
		accounts.value = Array.isArray(allUsers) ? allUsers : []
		defaultUserId.value = activeId ?? null
		await refreshAccount()
	} catch (e: any) {
		console.error('Failed to load accounts:', e)
	} finally {
		loading.value = false
	}
}

onMounted(() => {
	refresh()
})

async function handleMicrosoftLogin() {
	try {
		loggingIn.value = true
		errorMsg.value = null
		await login_flow()
		await refresh()
	} catch (e: any) {
		console.error('Microsoft login failed:', e)
		errorMsg.value = `Error en login de Microsoft: ${e?.message || e}`
	} finally {
		loggingIn.value = false
	}
}

async function handleAddOffline() {
	const name = offlineUsername.value.trim()
	if (!name) return
	try {
		loading.value = true
		errorMsg.value = null
		const newAcc = await add_offline_user(name)
		offlineUsername.value = ''
		showingOfflineInput.value = false
		if (newAcc?.profile?.id) {
			await set_default_user(newAcc.profile.id)
		}
		await refresh()
	} catch (e: any) {
		console.error('Failed to add offline account:', e)
		errorMsg.value = `Error al añadir cuenta offline: ${e?.message || e}`
	} finally {
		loading.value = false
	}
}

async function handleSelect(account: UserAccount) {
	try {
		await set_default_user(account.profile.id)
		await refresh()
	} catch (e: any) {
		console.error('Failed to set default account:', e)
	}
}

async function handleRemove(id: string) {
	try {
		await remove_user(id)
		await refresh()
	} catch (e: any) {
		console.error('Failed to remove account:', e)
	}
}

function getAvatarUrl(account: UserAccount) {
	if (account.profile?.id && account.refresh_token !== 'offline') {
		return `https://mc-heads.net/avatar/${account.profile.id}/64`
	}
	return `https://mc-heads.net/avatar/${encodeURIComponent(account.profile?.name || 'Steve')}/64`
}
</script>

<template>
	<div class="flex flex-col gap-6 max-w-2xl">
		<div>
			<h2 class="text-xl font-bold text-white mb-1">Cuentas de Minecraft</h2>
			<p class="text-sm text-[#98a2b3]">
				Gestiona tus cuentas de Minecraft Java oficiales de Microsoft o cuentas sin conexión (No-Premium).
			</p>
		</div>

		<!-- Error Banner -->
		<div v-if="errorMsg" class="p-3 rounded-xl bg-red-500/10 border border-red-500/30 text-red-400 text-sm">
			{{ errorMsg }}
		</div>

		<!-- Action Buttons -->
		<div class="flex flex-wrap gap-3">
			<Button
				type="colored"
				color="brand"
				:disabled="loggingIn || loading"
				class="!bg-[#2f65f6] hover:!bg-[#487bff] !text-white font-semibold"
				@click="handleMicrosoftLogin"
			>
				<SpinnerIcon v-if="loggingIn" class="animate-spin w-4 h-4 mr-2" />
				<LogInIcon v-else class="w-4 h-4 mr-2" />
				Añadir cuenta Microsoft
			</Button>

			<Button
				v-if="!showingOfflineInput"
				type="outlined"
				class="border-[#2a3245] hover:border-white/30 text-white"
				@click="showingOfflineInput = true"
			>
				<UserIcon class="w-4 h-4 mr-2" />
				Añadir cuenta No-Premium
			</Button>
		</div>

		<!-- Inline Form for Offline Account -->
		<div v-if="showingOfflineInput" class="p-4 rounded-xl bg-[#1c2230] border border-[#2a3245] flex flex-col gap-3">
			<span class="text-xs font-bold text-white uppercase tracking-wider">Nombre de jugador (Offline)</span>
			<div class="flex gap-2">
				<input
					v-model="offlineUsername"
					type="text"
					placeholder="Ej: fkdr3sh"
					class="flex-1 px-3 py-2 rounded-xl bg-[#0d1017] border border-[#2a3245] text-white text-sm focus:outline-none focus:border-[#2f65f6]"
					@keydown.enter="handleAddOffline"
				/>
				<Button
					type="colored"
					color="brand"
					:disabled="!offlineUsername.trim() || loading"
					class="!bg-[#2f65f6] hover:!bg-[#487bff] !text-white"
					@click="handleAddOffline"
				>
					Guardar
				</Button>
				<Button type="quiet" class="text-[#98a2b3]" @click="showingOfflineInput = false">
					Cancelar
				</Button>
			</div>
		</div>

		<!-- Accounts List -->
		<div class="flex flex-col gap-2">
			<span class="text-xs font-bold text-[#98a2b3] uppercase tracking-wider">
				Cuentas guardadas ({{ accounts.length }})
			</span>

			<div v-if="accounts.length === 0" class="p-8 rounded-xl bg-[#151923] border border-[#2a3245] text-center text-[#98a2b3] text-sm">
				No tienes ninguna cuenta añadida. Inicia sesión con Microsoft o añade una cuenta No-Premium arriba.
			</div>

			<div
				v-for="acc in accounts"
				:key="acc.profile.id"
				class="flex items-center justify-between p-3.5 rounded-xl border transition-all cursor-pointer"
				:class="
					defaultUserId === acc.profile.id
						? 'bg-[#1c2230] border-[#2f65f6] shadow-md'
						: 'bg-[#151923] border-[#2a3245] hover:border-white/20'
				"
				@click="handleSelect(acc)"
			>
				<div class="flex items-center gap-3">
					<div class="w-10 h-10 rounded-lg overflow-hidden bg-[#0d1017] shrink-0 border border-white/10">
						<img :src="getAvatarUrl(acc)" alt="" class="w-full h-full object-cover" />
					</div>
					<div>
						<div class="font-bold text-white text-sm flex items-center gap-2">
							{{ acc.profile.name }}
							<span
								v-if="defaultUserId === acc.profile.id"
								class="px-2 py-0.5 rounded-full bg-[#2f65f6]/20 text-[#2f65f6] text-[10px] font-bold border border-[#2f65f6]/30"
							>
								Activa
							</span>
						</div>
						<div class="text-xs text-[#98a2b3]">
							{{ acc.refresh_token === 'offline' ? 'Cuenta No-Premium (Sin conexión)' : 'Cuenta oficial Microsoft' }}
						</div>
					</div>
				</div>

				<div class="flex items-center gap-2">
					<IconButton
						type="quiet"
						color="red"
						label="Eliminar cuenta"
						class="text-[#98a2b3] hover:text-red-400 p-1"
						@click.stop="handleRemove(acc.profile.id)"
					>
						<TrashIcon class="w-4 h-4" />
					</IconButton>
				</div>
			</div>
		</div>
	</div>
</template>

