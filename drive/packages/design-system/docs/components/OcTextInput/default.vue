<template>
  <oc-text-input v-model="name" label="Your name" />
  <p>So your name is {{ name }}? Nice to meet you!</p>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const name = ref<string>('John Doe')
</script>
