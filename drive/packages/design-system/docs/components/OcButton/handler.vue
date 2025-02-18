<template>
  <oc-button @click="increaseCounter">Increase counter</oc-button>
  <oc-button class="oc-ml-m" @click="resetCounter">Reset counter</oc-button>
  <div class="oc-mt-m">Current count: {{ counter }}</div>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const counter = ref(0)

const increaseCounter = () => {
  counter.value++
}

const resetCounter = () => {
  counter.value = 0
}
</script>
