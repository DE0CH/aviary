<template>
  <oc-textarea v-model="text" label="Lorem?" />

  <p>Value: {{ text }}</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const text = ref<string>('Ipsum!')
</script>
