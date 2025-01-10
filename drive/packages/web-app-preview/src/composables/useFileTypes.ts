import { Resource } from '@opencloud-eu/web-client'

export const useFileTypes = () => {
  const isFileTypeImage = (file: Resource) => {
    return !isFileTypeAudio(file) && !isFileTypeVideo(file)
  }
  const isFileTypeAudio = (file: Resource) => {
    return file.mimeType.toLowerCase().startsWith('audio')
  }
  const isFileTypeVideo = (file: Resource) => {
    return file.mimeType.toLowerCase().startsWith('video')
  }

  return {
    isFileTypeImage,
    isFileTypeAudio,
    isFileTypeVideo
  }
}
