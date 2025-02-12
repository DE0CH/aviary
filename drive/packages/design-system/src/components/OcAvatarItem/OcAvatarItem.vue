<template>
  <div
    :data-test-item-name="name"
    :aria-label="accessibleLabel === '' ? null : accessibleLabel"
    :aria-hidden="accessibleLabel === '' ? 'true' : null"
    :focusable="accessibleLabel === '' ? 'false' : null"
    :role="accessibleLabel === '' ? null : 'img'"
  >
    <span
      class="oc-avatar-item"
      :style="{
        backgroundColor,
        '--icon-color': iconColor,
        '--width': avatarWidth
      }"
    >
      <oc-icon v-if="hasIcon" :name="icon" :size="iconSize" :fill-type="iconFillType" />
    </span>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import OcIcon from '../OcIcon/OcIcon.vue'

export interface Props {
  name: string
  accessibleLabel?: string
  background?: string
  icon?: string
  iconColor?: string
  iconFillType?: 'fill' | 'line' | 'none'
  iconSize?: 'xsmall' | 'small' | 'medium' | 'large' | 'xlarge' | 'xxlarge' | 'xxxlarge'
  width?: number
}

const {
  name,
  accessibleLabel = '',
  background = 'var(--oc-color-swatch-passive-default)',
  icon,
  iconColor = 'var(--oc-color-text-inverse)',
  iconFillType = 'fill',
  iconSize = 'small',
  width = 30
} = defineProps<Props>()

const avatarWidth = computed(() => width + 'px')
const hasIcon = computed(() => icon !== null)
const backgroundColor = computed(() => background || pickBackgroundColor())

const pickBackgroundColor = () => {
  const backgroundColors = [
    '#b82015',
    '#c21c53',
    '#9C27B0',
    '#673AB7',
    '#3F51B5',
    '#106892',
    '#055c68',
    '#208377',
    '#1a761d',
    '#476e1a',
    '#636d0b',
    '#8e5c11',
    '#795548',
    '#465a64'
  ]
  return backgroundColors[Math.floor(Math.random() * backgroundColors.length)]
}
</script>

<style lang="scss">
.oc-avatar-item {
  align-items: center;
  background-position: center;
  background-repeat: no-repeat;
  background-size: 18px;
  border-radius: 50%;
  display: inline-flex;
  height: var(--width);
  justify-content: center;
  width: var(--width);

  .oc-icon > svg {
    fill: var(--icon-color) !important;
  }
}
</style>
