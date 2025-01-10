import { useFileActions } from './files'
import { Resource, SpaceResource } from '@opencloud-eu/web-client'

export function useOpenWithDefaultApp() {
  const { getDefaultAction } = useFileActions()

  const openWithDefaultApp = ({
    space,
    resource
  }: {
    space: SpaceResource
    resource: Resource
  }) => {
    if (!resource || resource.isFolder) {
      return
    }

    const fileActionsOptions = {
      resources: [resource],
      space: space
    }
    const defaultEditorAction = getDefaultAction({ ...fileActionsOptions, omitSystemActions: true })
    if (defaultEditorAction) {
      defaultEditorAction.handler(fileActionsOptions)
    }
  }

  return { openWithDefaultApp }
}
