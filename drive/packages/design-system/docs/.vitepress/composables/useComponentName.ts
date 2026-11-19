import { useData } from 'vitepress'
import { computed, unref } from 'vue'

export const useComponentName = () => {
  const data = useData()

  const componentName = computed(() => {
    // extract component name from file path
    const filePath = unref(data.page).filePath
    return filePath.substring(filePath.lastIndexOf('/') + 1, filePath.lastIndexOf('.'))
  })

  return {
    componentName
  }
}
