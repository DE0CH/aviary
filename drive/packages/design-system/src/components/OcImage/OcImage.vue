<template>
  <img :src="src" :alt="alt" :aria-hidden="`${ariaHidden}`" :title="title" :loading="loadingType" />
</template>

<script setup lang="ts">
import { computed } from 'vue'

export interface Props {
  /**
   * @docs The image source URL.
   */
  src: string
  /**
   * @docs The image alt text. Should be provided for screen readers.
   */
  alt?: string
  /**
   * @docs The loading type of the image.
   * @default eager
   */
  loadingType?: 'lazy' | 'eager'
  /**
   * @docs The image title attribute.
   */
  title?: string
}

const { src, alt = '', title, loadingType = 'eager' } = defineProps<Props>()

const ariaHidden = computed(() => alt.length === 0)
</script>
