<template>
  <span>
    <input
      :id="id"
      v-model="model"
      type="radio"
      name="radio"
      :class="classes"
      :aria-checked="option === modelValue"
      :value="option"
      :disabled="disabled"
    />
    <label :for="id" :class="labelClasses" v-text="label" />
  </span>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { getSizeClass, uniqueId } from '../../helpers'

export interface Props {
  label: string
  disabled?: boolean
  hideLabel?: boolean
  id?: string
  modelValue?: unknown
  option?: unknown
  size?: 'small' | 'medium' | 'large'
}

const {
  label,
  disabled = false,
  hideLabel = false,
  id = uniqueId('oc-radio-'),
  modelValue = false,
  option,
  size = 'medium'
} = defineProps<Props>()

const emit = defineEmits(['update:modelValue'])

const model = computed({
  get: () => modelValue,
  set: (value: unknown) => emit('update:modelValue', value)
})

const classes = computed(() => ['oc-radio', 'oc-radio-' + getSizeClass(size)])

const labelClasses = computed(() => ({
  'oc-invisible-sr': hideLabel,
  'oc-cursor-pointer': !disabled
}))
</script>

<style lang="scss">
@mixin oc-form-check-size($factor) {
  height: $oc-size-form-check-default * $factor;
  width: $oc-size-form-check-default * $factor;
}

.oc-radio {
  -webkit-appearance: none;
  -moz-appearance: none;

  border: 1px solid var(--oc-color-swatch-brand-default);
  border-radius: 50%;
  box-sizing: border-box;
  background-color: var(--oc-color-input-bg);
  background-position: 50% 50%;
  background-repeat: no-repeat;

  display: inline-block;
  margin: 0;
  overflow: hidden;

  transition: 0.2s ease-in-out;
  transition-property: background-color, border;
  vertical-align: middle;
  width: 1rem;

  &:not(:disabled) {
    cursor: pointer;
  }

  &:checked {
    background-color: var(--oc-color-background-highlight) !important;
  }

  &.oc-radio-s {
    @include oc-form-check-size(0.7);
  }

  &.oc-radio-m {
    @include oc-form-check-size(1);
  }

  &.oc-radio-l {
    @include oc-form-check-size(1.5);
  }
}

label > .oc-radio + span {
  margin-left: var(--oc-space-xsmall);
}
</style>
