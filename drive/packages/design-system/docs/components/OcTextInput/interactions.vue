<template>
  <oc-button class="oc-my-m" @click="focus">Focus input below</oc-button>
  <oc-text-input ref="inputForFocusRef" label="Focus field" />
  <oc-button class="oc-my-m" @click="focusAndSelect">Focus and select input below</oc-button>
  <oc-text-input
    ref="inputForFocusSelectRef"
    label="Select field"
    value="Will you select this existing text?"
  />
  <oc-text-input v-model="inputValueForClearing" label="Clear input" :clear-button-enabled="true" />
  <oc-text-input
    v-model="inputValueWithDefault"
    label="Input with default"
    :clear-button-enabled="true"
    default-value="Some default"
  />

  <p>Value: {{ inputValueWithDefault !== null ? inputValueWithDefault : 'null' }}</p>
</template>

<script setup lang="ts">
import { ref, unref } from 'vue'

const inputValueForClearing = ref<string>('clear me')
const inputValueWithDefault = ref<string | null>(null)

const inputForFocusRef = ref(null)
const focus = () => {
  unref(inputForFocusRef).focus()
}
const inputForFocusSelectRef = ref(null)
const focusAndSelect = () => {
  unref(inputForFocusSelectRef).focus()
}
</script>
