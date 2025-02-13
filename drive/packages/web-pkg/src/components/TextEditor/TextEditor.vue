<template>
  <div id="text-editor-container" class="oc-height-1-1">
    <md-preview
      v-if="isReadOnly"
      id="text-editor-preview-component"
      :model-value="currentContent"
      :language="languages[language.current] || 'en-US'"
      :theme="theme"
      read-only
      :toolbars="[]"
    />
    <md-editor
      v-else
      id="text-editor-component"
      :model-value="currentContent"
      :language="languages[language.current] || 'en-US'"
      :theme="theme"
      :preview="isMarkdown"
      :toolbars="isMarkdown ? undefined : []"
      :read-only="isReadOnly"
      @on-change="(value) => $emit('update:currentContent', value)"
    />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, unref, PropType } from 'vue'
import { Resource } from '@opencloud-eu/web-client'

import { config, MdEditor, MdPreview } from 'md-editor-v3'
import 'md-editor-v3/lib/style.css'

import { languageUserDefined, languages } from './l18n'

import { useGettext } from 'vue3-gettext'
import { useThemeStore } from '../../composables'
import { AppConfigObject } from '../../apps'

export default defineComponent({
  name: 'TextEditor',
  components: { MdEditor, MdPreview },
  props: {
    applicationConfig: { type: Object as PropType<AppConfigObject>, required: false },
    currentContent: {
      type: String,
      required: true
    },
    markdownMode: { type: Boolean, required: false, default: false },
    isReadOnly: { type: Boolean, required: false, default: false },
    resource: { type: Object as PropType<Resource>, required: false }
  },
  emits: ['update:currentContent'],
  setup(props) {
    const language = useGettext()
    const { currentTheme } = useThemeStore()

    // Should not be a ref, otherwise functions like setMarkdown won't work
    const editorConfig = computed(() => {
      // TODO: Remove typecasting once vue-tsc has figured it out
      const { showPreviewOnlyMd = true } = props.applicationConfig as AppConfigObject
      return { showPreviewOnlyMd }
    })

    const isMarkdown = computed(() => {
      return (
        props.markdownMode ||
        ['md', 'markdown'].includes(props.resource?.extension) ||
        !unref(editorConfig).showPreviewOnlyMd
      )
    })

    const theme = computed(() => (unref(currentTheme).isDark ? 'dark' : 'light'))

    config({
      editorConfig: {
        languageUserDefined
      }
    })

    return {
      isMarkdown,
      theme,
      language,
      languages
    }
  }
})
</script>
<style lang="scss">
.md-editor {
  height: 100%;

  &-code-head {
    justify-content: end !important;
  }

  &-code-flag {
    display: none;
  }
}
</style>
