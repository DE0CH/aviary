<template>
  <oc-text-input v-model="inputValue" label="Text" />
  <oc-text-input v-model="inputValue" disabled label="Text" />
</template>

<script setup lang="ts">
import { ref } from 'vue'

const inputValue = ref<string>('initial')
</script>
