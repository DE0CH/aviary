import { computed, markRaw } from 'vue'
import { preferencesPanelExtensionPoint, progressBarExtensionPoint } from './extensionPoints'
import AppTokens from './components/Account/AppTokens.vue'
import { CustomComponentExtension, LoadingIndicator } from '@opencloud-eu/web-pkg'

const $gettext = (str: string) => str

export const extensions = () => {
  return computed(() => [
    {
      id: 'com.github.opencloud-eu.web.runtime.preferences-panels.app-tokens',
      type: 'customComponent',
      extensionPointIds: [preferencesPanelExtensionPoint.id],
      content: AppTokens
    } as CustomComponentExtension,
    {
      id: 'com.github.opencloud-eu.web.runtime.default-progress-bar',
      type: 'customComponent',
      extensionPointIds: [progressBarExtensionPoint.id],
      content: markRaw(LoadingIndicator),
      userPreference: {
        optionLabel: $gettext('Default progress bar')
      }
    } as CustomComponentExtension
  ])
}
