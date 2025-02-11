<template>
  <span
    :id="id"
    class="oc-invisible-sr oc-hidden-announcer"
    :aria-live="level"
    aria-atomic="true"
    v-text="announcement"
  />
</template>

<script setup lang="ts">
import { HTMLAttributes, computed } from 'vue'

export interface Props {
  announcement: string
  level?: HTMLAttributes['aria-live']
}

const { announcement, level = 'polite' } = defineProps<Props>()

const id = computed(
  () => Math.random().toString(36).substring(2, 15) + Math.random().toString(36).substring(2, 15)
)
</script>
