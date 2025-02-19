<template>
  <oc-select v-model="option" label="Please select an option" :options="options" />
  <p>Selected option: {{ option }}</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const option = ref()
const options = [
  { value: '1', label: 'Option 1' },
  { value: '2', label: 'Option 2' },
  { value: '3', label: 'Option 3' }
]
</script>
