<template>
  <oc-filter-chip
    filter-label="Toggle filter"
    :is-toggle="true"
    :is-toggle-active="filterActive"
    @toggle-filter="toggleFilter"
    @clear-filter="toggleFilter"
  />
</template>

<script setup lang="ts">
import { ref, unref } from 'vue'

const filterActive = ref(false)

const toggleFilter = () => {
  filterActive.value = !unref(filterActive)

  // handle filter change...
}
</script>
