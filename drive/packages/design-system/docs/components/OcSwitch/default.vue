<template>
  <oc-switch v-model:checked="isChecked" label="Toggle switch" />
  <p>The switch is turned: {{ isChecked }}</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const isChecked = ref(false)
</script>
