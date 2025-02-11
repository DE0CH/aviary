<template>
  <span
    class="oc-avatar-count"
    :style="{ width: size + 'px', height: size + 'px', fontSize: fontSize }"
    v-text="`+${count}`"
  />
</template>

<script setup lang="ts">
import { computed } from 'vue'

export interface Props {
  count: number
  size?: number
}

const { count, size = 30 } = defineProps<Props>()

const fontSize = computed(() => {
  return Math.floor(size / 2.5) + 'px'
})
</script>

<style lang="scss">
.oc-avatar-count {
  align-items: center;
  background-color: var(--oc-color-swatch-passive-default);
  border-radius: 50%;
  color: var(--oc-color-text-inverse);
  display: flex;
  justify-content: center;
}
</style>
