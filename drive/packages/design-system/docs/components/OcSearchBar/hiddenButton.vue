<template>
  <oc-search-bar
    label="Search files"
    placeholder="Search files"
    :button-hidden="true"
    @search="onSearch"
  />
  <p>Search query: {{ searchQuery }}</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const searchQuery = ref<string>()

const onSearch = (val: string) => {
  searchQuery.value = val
}
</script>
