<template>
  <div id="preferences-panel-app-tokens">
    <div class="oc-flex oc-flex-middle oc-flex-space-between">
      <h2 v-text="$gettext('App tokens')" />
      <oc-button
        v-if="!authAppServiceDisabled"
        size="small"
        class="create-app-token-btn oc-ml-m"
        @click="openCreateAppTokenModal"
      >
        <oc-icon name="add" size="small" />
        <span v-text="$gettext('New')" />
      </oc-button>
    </div>
    <span
      v-if="authAppServiceDisabled"
      data-testid="auth-service-unavailable"
      v-text="
        $gettext(
          'App tokens are not available because the auth app service is not running. Please contact an administrator of your instance.'
        )
      "
    />
    <span
      v-else-if="!appTokens.length"
      data-testid="no-app-tokens-available"
      v-text="$gettext('No app tokens available.')"
    />
    <div v-else>
      <oc-table :data="visibleAppTokens" :fields="tableFields">
        <template #token="{ item }">
          <div class="oc-flex oc-flex-middle">
            <div class="oc-width-1-1 oc-text-truncate">
              <span v-text="item.token" />
            </div>
            <oc-button
              v-oc-tooltip="$gettext('Copy app token')"
              appearance="raw"
              class="copy-app-token-btn oc-p-xs oc-ml-m"
              :aria-label="$gettext('Copy app token')"
              @click="copyAppToken(item.token)"
            >
              <oc-icon name="file-copy" fill-type="line" />
            </oc-button>
          </div>
        </template>
        <template #creationDate="{ item }">
          <div class="oc-width-1-1 oc-text-truncate">
            <span v-text="formatDateFromISO(item.created_date, currentLanguage)" />
          </div>
        </template>
        <template #exprationDate="{ item }">
          <div class="oc-width-1-1 oc-text-truncate">
            <span v-text="formatDateFromISO(item.expiration_date, currentLanguage)" />
          </div>
        </template>
        <template #actions="{ item }">
          <oc-button
            v-oc-tooltip="$gettext('Delete app token')"
            appearance="raw"
            class="delete-app-token-btn oc-p-xs oc-ml-m"
            :aria-label="$gettext('Delete app token')"
            @click="openDeleteAppTokenModal(item)"
          >
            <oc-icon name="delete-bin-5" fill-type="line" />
          </oc-button>
        </template>
      </oc-table>
      <div
        v-if="appTokens.length > TOKENS_TO_DISPLAY"
        class="oc-width-1-1 oc-flex oc-flex-center oc-mt-m"
      >
        <oc-button appearance="raw" @click="listExpanded = !listExpanded">
          <span v-text="listExpanded ? $gettext('Show less') : $gettext('Show more')" />
          <oc-icon :name="'arrow-' + (listExpanded ? 'up' : 'down') + '-s'" fill-type="line" />
        </oc-button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { call } from '@opencloud-eu/web-client'
import {
  DatePickerModal,
  formatDateFromISO,
  useClientService,
  useMessages,
  useModals
} from '@opencloud-eu/web-pkg'
import { computed, onMounted, onUnmounted, Ref, ref, unref } from 'vue'
import { useTask } from 'vue-concurrency'
import { useGettext } from 'vue3-gettext'
import { useClipboard } from '@vueuse/core'
import { z } from 'zod'
import { DateTime } from 'luxon'

const AppTokenSchema = z.object({
  token: z.string(),
  expiration_date: z.string(),
  created_date: z.string(),
  label: z.string().optional()
})

const RawAppTokenSchema = z.array(AppTokenSchema)

export type AppToken = z.infer<typeof AppTokenSchema>

const { $gettext, current: currentLanguage } = useGettext()
const { dispatchModal } = useModals()
const { showMessage, showErrorMessage } = useMessages()
const { httpAuthenticated: client } = useClientService()
const { copy } = useClipboard({ legacy: true, copiedDuring: 550 })

const appTokens: Ref<AppToken[]> = ref([])
const authAppServiceDisabled = ref<boolean | undefined>()
const listExpanded = ref(false)

const TOKENS_TO_DISPLAY = 5

const loadTokensTask = useTask(function* (signal) {
  try {
    const { data } = yield* call(client.get<AppToken[]>('/auth-app/tokens', { signal }))
    const tokens = RawAppTokenSchema.parse(data)
    appTokens.value = tokens.sort((a, b) => b.created_date.localeCompare(a.created_date))
    authAppServiceDisabled.value = false
  } catch (error) {
    console.error(error)
    // FIXME: check the service availability beforehand when the server supports it
    authAppServiceDisabled.value = true
  }
}).restartable()

const createAppToken = async (expiry: string) => {
  try {
    const { data } = await client.post('/auth-app/tokens', null, { params: { expiry } })
    unref(appTokens).unshift(data)
    showMessage({ title: $gettext('The app token has been created.') })
  } catch (error) {
    console.error(error)
    showErrorMessage({
      title: $gettext('An error occurred while creating the app token.'),
      errors: [error]
    })
  }
}

const deleteAppToken = async ({ token }: AppToken) => {
  try {
    await client.delete('/auth-app/tokens', { params: { token } })
    appTokens.value = unref(appTokens).filter((appToken) => appToken.token !== token)
    showMessage({ title: $gettext('The app token has been deleted.') })
  } catch (error) {
    console.error(error)
    showErrorMessage({
      title: $gettext('An error occurred while deleting the app token.'),
      errors: [error]
    })
  }
}

const openCreateAppTokenModal = () => {
  dispatchModal({
    title: $gettext('Create a new app token'),
    confirmText: $gettext('Create'),
    customComponent: DatePickerModal,
    hideActions: true,
    customComponentAttrs: () => ({
      minDate: DateTime.now()
    }),
    onConfirm: (expiryDate: DateTime) => {
      const diffInHours = expiryDate.diff(DateTime.now(), 'hours').hours
      createAppToken(`${diffInHours}h`)
    }
  })
}

const openDeleteAppTokenModal = (appToken: AppToken) => {
  dispatchModal({
    title: $gettext('Delete app token'),
    confirmText: $gettext('Delete'),
    message: $gettext('Are you sure you want to delete this app token?'),
    onConfirm: () => deleteAppToken(appToken)
  })
}

const copyAppToken = (token: string) => {
  copy(token)
  showMessage({ title: $gettext('The app token has been copied to your clipboard.') })
}

const visibleAppTokens = computed(() => {
  if (unref(listExpanded)) {
    return unref(appTokens)
  }
  return unref(appTokens).slice(0, TOKENS_TO_DISPLAY)
})

const tableFields = computed(() => {
  return [
    {
      name: 'token',
      type: 'slot',
      width: 'expand',
      wrap: 'truncate',
      title: $gettext('App Token')
    },
    {
      name: 'creationDate',
      type: 'slot',
      wrap: 'truncate',
      title: $gettext('Created at')
    },
    {
      name: 'exprationDate',
      type: 'slot',
      wrap: 'truncate',
      title: $gettext('Expires at')
    },
    {
      name: 'actions',
      type: 'slot',
      alignH: 'right',
      title: $gettext('Actions')
    }
  ]
})

onMounted(() => {
  loadTokensTask.perform()
})

onUnmounted(() => {
  if (loadTokensTask.isRunning) {
    loadTokensTask.cancelAll()
  }
})
</script>

<style lang="scss" scoped>
.delete-app-token-btn,
.copy-app-token-btn {
  &:hover {
    background-color: var(--oc-color-background-hover) !important;
    border-radius: 3px;
  }
}
</style>
