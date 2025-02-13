<template>
  <table :class="tableClasses">
    <slot />
  </table>
</template>

<script setup lang="ts">
import { computed } from 'vue'

export interface Props {
  hover?: boolean
}
const { hover = false } = defineProps<Props>()

const tableClasses = computed(() => {
  const result = ['oc-table-simple']
  if (hover) {
    result.push('oc-table-simple-hover')
  }
  return result
})
</script>

<style lang="scss">
.oc-table-simple {
  border-collapse: collapse;
  border-spacing: 0;
  width: 100%;

  &-hover tr {
    transition: background-color $transition-duration-short ease-in-out;
  }

  tr + tr {
    border-top: 1px solid var(--oc-color-border);
  }

  &-hover tr:hover {
    background-color: var(--oc-color-input-border);
  }
}
</style>
