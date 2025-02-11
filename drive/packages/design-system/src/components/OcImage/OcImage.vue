<template>
  <img :src="src" :alt="alt" :aria-hidden="`${ariaHidden}`" :title="title" :loading="loadingType" />
</template>

<script setup lang="ts">
import { computed } from 'vue'

export interface Props {
  src: string
  alt?: string
  loadingType?: 'lazy' | 'eager'
  title?: string
}

const { src, alt = '', title, loadingType = 'eager' } = defineProps<Props>()

const ariaHidden = computed(() => alt.length === 0)
</script>
