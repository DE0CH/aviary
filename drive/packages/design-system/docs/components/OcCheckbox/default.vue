<template>
  <oc-checkbox v-model="inputValue" label="Some checkbox" />

  <p>Checked: {{ inputValue }}</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const inputValue = ref(false)
</script>
