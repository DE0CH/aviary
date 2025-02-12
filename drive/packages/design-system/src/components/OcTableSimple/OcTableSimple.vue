<template>
  <table :class="tableClasses">
    <slot />
  </table>
</template>
<script lang="ts">
import { defineComponent } from 'vue'

/**
 * A table component with manually built layout.
 */
export default defineComponent({
  name: 'OcTableSimple',
  status: 'ready',
  release: '2.1.0',
  props: {
    /**
     * Whether or not table rows should be highlighted when hovered.
     */
    hover: {
      type: Boolean,
      default: false
    }
  },
  computed: {
    tableClasses() {
      const result = ['oc-table-simple']
      if (this.hover) {
        result.push('oc-table-simple-hover')
      }
      return result
    }
  }
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
