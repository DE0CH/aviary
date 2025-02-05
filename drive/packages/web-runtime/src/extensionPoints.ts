import {
  AppMenuItemExtension,
  CustomComponentExtension,
  Extension,
  ExtensionPoint
} from '@opencloud-eu/web-pkg'
import { computed } from 'vue'

export const appMenuExtensionPoint: ExtensionPoint<AppMenuItemExtension> = {
  id: 'app.runtime.header.app-menu',
  extensionType: 'appMenuItem',
  multiple: true
}

export const preferencesPanelExtensionPoint: ExtensionPoint<CustomComponentExtension> = {
  id: 'app.runtime.preferences.panels',
  extensionType: 'customComponent',
  multiple: true
}

export const topBarCenterExtensionPoint: ExtensionPoint<CustomComponentExtension> = {
  id: 'app.runtime.header.center',
  extensionType: 'customComponent',
  multiple: true
}

export const extensionPoints = () => {
  return computed<ExtensionPoint<Extension>[]>(() => {
    return [appMenuExtensionPoint, preferencesPanelExtensionPoint, topBarCenterExtensionPoint]
  })
}
