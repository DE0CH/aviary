<template>
  <oc-button id="drop-2-btn">Open drop</oc-button>
  <oc-info-drop toggle="#drop-2-btn" title="Some title" :list="list" />
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { ContextualHelperDataListItem } from '@opencloud-eu/design-system/helpers'

const list = computed<ContextualHelperDataListItem[]>(() => [
  {
    headline: true,
    text: 'Lorem ipsum'
  },
  {
    headline: false,
    text: 'dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod.'
  },
  {
    headline: true,
    text: 'tempor invidunt'
  },
  {
    headline: false,
    text: 'ut labore et dolore magna aliquyam erat, sed diam voluptua.'
  }
])
</script>
