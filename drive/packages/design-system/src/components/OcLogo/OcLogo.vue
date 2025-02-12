<template>
  <oc-image class="oc-logo" :src="src" :alt="alt" />
</template>

<script setup lang="ts">
import OcImage from '../OcImage/OcImage.vue'

export interface Props {
  alt: string
  src: string
}

const { alt, src } = defineProps<Props>()
</script>

<style lang="scss">
.oc-logo {
  max-width: var(--oc-size-max-width-logo);
  max-height: var(--oc-size-max-height-logo);
  margin: var(--oc-space-small);
}
</style>
