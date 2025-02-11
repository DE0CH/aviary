<template>
  <ul class="oc-list oc-my-rm oc-mx-rm" :class="{ 'oc-list-raw': raw }">
    <slot />
  </ul>
</template>

<script setup lang="ts">
export interface Props {
  raw?: boolean
}

const { raw = false } = defineProps<Props>()
</script>

<style lang="scss">
.oc-list {
  list-style: none;
  margin: 0;
  padding: 0;

  &-divider > :nth-child(n + 2) {
    border-top: 1px solid var(--oc-color-border);
    margin-top: var(--oc-space-small);
    padding-top: var(--oc-space-small);
  }
  &-raw {
    list-style-type: none;
    a {
      text-decoration: none !important;
      &:hover {
        color: inherit;
      }
    }
  }
}
</style>
