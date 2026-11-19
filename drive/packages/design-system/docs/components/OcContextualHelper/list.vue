<template>
  <p>
    I have some structured, additional context.
    <oc-contextual-helper title="Some helper" :list="list" />
  </p>
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
