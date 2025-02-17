<template>
  <oc-text-input
    label="Input with description message below"
    class="oc-mb-s"
    description-message="This is a description message."
    :fix-message-line="true"
  />
  <oc-text-input
    v-model="valueForMessages"
    label="Input with error and warning messages with reserved space below"
    class="oc-mb-s"
    :error-message="errorMessage"
    :warning-message="warningMessage"
    :fix-message-line="true"
  />
  <oc-text-input
    v-model="valueForMessages"
    label="Input with error and warning messages without reserved space below"
    class="oc-mb-s"
    :error-message="errorMessage"
    :warning-message="warningMessage"
  />
</template>

<script setup lang="ts">
import { computed, ref, unref } from 'vue'

const valueForMessages = ref<string>('')

const errorMessage = computed(() => {
  return unref(valueForMessages).length === 0 ? 'Value is required.' : ''
})
const warningMessage = computed(() => {
  return unref(valueForMessages).endsWith(' ') ? 'Trailing whitespace should be avoided.' : ''
})
</script>
