<template>
  <div
    :class="classes"
    :aria-valuemax="max"
    :aria-valuenow="value"
    aria-busy="true"
    aria-valuemin="0"
    role="progressbar"
  >
    <div v-if="!indeterminate" class="oc-progress-current" :style="{ width: progressValue }"></div>
    <div v-else class="oc-progress-indeterminate">
      <div class="oc-progress-indeterminate-first"></div>
      <div class="oc-progress-indeterminate-second"></div>
    </div>
  </div>
</template>
<script setup lang="ts">
import { computed } from 'vue'

export interface Props {
  indeterminate?: boolean
  max?: number
  size?: 'default' | 'small'
  value?: number
  variation?: 'primary' | 'passive' | 'danger' | 'success' | 'warning'
}

const {
  indeterminate = false,
  max,
  size = 'default',
  value = 0,
  variation = 'primary'
} = defineProps<Props>()

const classes = computed(() => {
  return `oc-progress oc-progress-${size} oc-progress-${variation}`
})

const progressValue = computed(() => {
  if (!max) {
    return '-'
  }
  const num = (value / max) * 100
  return `${num}%`
})
</script>

<style lang="scss">
$progress-height: 15px !default;
$progress-height-small: 5px !default;

.oc-progress {
  background-color: var(--oc-color-background-highlight);
  display: block;
  height: $progress-height;
  // Add the correct vertical alignment in Chrome, Firefox, and Opera.
  width: 100%;
  position: relative;
  overflow-x: hidden;

  &-small {
    height: $progress-height-small;
  }
  &-current {
    height: 100%;
    position: absolute;
    transition: width 0.5s;
  }
  &-indeterminate div {
    height: 100%;
    position: absolute;
  }
  &-indeterminate-first {
    animation-duration: 2s;
    animation-name: indeterminate-first;
    animation-iteration-count: infinite;
  }
  &-indeterminate-second {
    animation-duration: 2s;
    animation-delay: 0.5s;
    animation-name: indeterminate-second;
    animation-iteration-count: infinite;
  }

  @keyframes indeterminate-first {
    from {
      left: -10%;
      width: 10%;
    }
    to {
      left: 120%;
      width: 100%;
    }
  }

  @keyframes indeterminate-second {
    from {
      left: -100%;
      width: 80%;
    }
    to {
      left: 110%;
      width: 10%;
    }
  }

  &-primary &-current,
  &-primary &-indeterminate div {
    background-color: var(--oc-color-swatch-primary-default);
  }
  &-passive &-current,
  &-passive &-indeterminate div {
    background-color: var(--oc-color-swatch-passive-default);
  }
  &-success &-current,
  &-success &-indeterminate div {
    background-color: var(--oc-color-swatch-success-default);
  }
  &-warning &-current,
  &-warning &-indeterminate div {
    background-color: var(--oc-color-swatch-warning-default);
  }
  &-danger &-current,
  &-danger &-indeterminate div {
    background-color: var(--oc-color-swatch-danger-default);
  }
}
</style>
