<template>
  <oc-select
    v-model="selectedOptions"
    label="Please select one or more options"
    :options="options"
    :multiple="true"
  />
  <p>Selected options: {{ selectedOptions }}</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const selectedOptions = ref([])
const options = [
  { value: '1', label: 'Option 1' },
  { value: '2', label: 'Option 2' },
  { value: '3', label: 'Option 3' }
]
</script>
