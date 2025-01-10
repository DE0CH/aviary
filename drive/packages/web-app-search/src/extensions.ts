import { computed } from 'vue'
import { CustomComponentExtension, Extension } from '@opencloud-eu/web-pkg'
import SearchBar from './portals/SearchBar.vue'

const searchBarExtension: CustomComponentExtension = {
  id: 'com.github.opencloud-eu.web.search.search-bar',
  type: 'customComponent',
  extensionPointIds: ['app.runtime.header.center'],
  content: SearchBar
}

export const extensions = () => {
  return computed<Extension[]>(() => {
    return [searchBarExtension]
  })
}
