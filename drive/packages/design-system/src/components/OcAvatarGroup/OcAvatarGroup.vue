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
import { SizeType } from '../../helpers'
import OcAvatarItem from '../OcAvatarItem/OcAvatarItem.vue'

export interface Props {
  name: string
  accessibleLabel?: string
  iconSize?: SizeType
  width?: number
}

const { name, accessibleLabel = '', iconSize = 'small', width = 30 } = defineProps<Props>()
</script>
