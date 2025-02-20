export const useLoadMetaData = () => {
  const load = async (type: string, componentName: string) => {
    try {
      const response = await fetch(`../${type}/${componentName}.json`)
      if (response.headers?.get('content-type') !== 'application/json') {
        throw new Error(`file "${type}/${componentName}.json" does not exist`)
      }
      return response.json()
    } catch (e) {
      console.error(`loading ${type}:`, e)
    }
  }

  const loadEmits = async (componentName: string) => {
    return load('emits', componentName)
  }

  const loadProps = async (componentName: string) => {
    return load('props', componentName)
  }

  const loadSlots = async (componentName: string) => {
    return load('slots', componentName)
  }

  return {
    loadEmits,
    loadProps,
    loadSlots
  }
}
