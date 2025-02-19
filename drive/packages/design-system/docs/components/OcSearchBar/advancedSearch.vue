<template>
  <oc-search-bar
    label="Search files"
    placeholder="Search files"
    :button-hidden="true"
    @search="onSearch"
    @advanced-search="advancedSearch"
  />
  <p>Search query: {{ searchQuery }}</p>
  <p v-if="advancedSearchTriggered">Advanced search has been triggered.</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const searchQuery = ref<string>()
const advancedSearchTriggered = ref(false)

const onSearch = (val: string) => {
  searchQuery.value = val
}

const advancedSearch = () => {
  // handle advaned search...
  advancedSearchTriggered.value = true
}
</script>
