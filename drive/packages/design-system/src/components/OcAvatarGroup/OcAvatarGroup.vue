<template>
  <oc-avatar-item
    :width="width"
    :icon-size="iconSize"
    icon="group"
    :name="name"
    :accessible-label="accessibleLabel"
  />
</template>

<script setup lang="ts">
import OcAvatarItem from '../OcAvatarItem/OcAvatarItem.vue'

export interface Props {
  name: string
  accessibleLabel?: string
  iconSize?: string
  width?: number
}

const { name, accessibleLabel = '', iconSize = 'small', width = 30 } = defineProps<Props>()
</script>
