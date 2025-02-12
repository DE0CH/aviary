<template>
  <div :class="$attrs.class">
    <slot name="label">
      <label class="oc-label" :for="id" v-text="label" />
    </slot>
    <div class="oc-position-relative">
      <oc-icon
        v-if="readOnly"
        name="lock"
        size="small"
        class="oc-mt-s oc-ml-s oc-position-absolute"
      />
      <component
        :is="inputComponent"
        :id="id"
        v-bind="additionalAttributes"
        ref="inputRef"
        :aria-invalid="ariaInvalid"
        class="oc-text-input oc-input oc-rounded"
        :class="{
          'oc-text-input-warning': !!warningMessage,
          'oc-text-input-danger': !!errorMessage,
          'oc-pl-l': !!readOnly,
          'clear-action-visible': showClearButton
        }"
        :type="type"
        :value="modelValue"
        :disabled="disabled || readOnly"
        v-on="additionalListeners"
        @change="onChange(($event.target as HTMLInputElement).value)"
        @input="onInput(($event.target as HTMLInputElement).value)"
        @password-challenge-completed="$emit('passwordChallengeCompleted')"
        @password-challenge-failed="$emit('passwordChallengeFailed')"
        @focus="onFocus($event.target)"
      />
      <oc-button
        v-if="showClearButton"
        :aria-label="clearButtonAccessibleLabelValue"
        class="oc-pr-s oc-position-center-right oc-text-input-btn-clear"
        appearance="raw"
        @click="onClear"
      >
        <oc-icon name="close" size="small" variation="passive" />
      </oc-button>
    </div>
    <div
      v-if="showMessageLine"
      class="oc-text-input-message"
      :class="{
        'oc-text-input-description': !!descriptionMessage,
        'oc-text-input-warning': !!warningMessage,
        'oc-text-input-danger': !!errorMessage
      }"
    >
      <oc-icon
        v-if="messageText !== null && !!descriptionMessage"
        name="information"
        size="small"
        fill-type="line"
        accessible-label="info"
        aria-hidden="true"
      />

      <span
        :id="messageId"
        :class="{
          'oc-text-input-description': !!descriptionMessage,
          'oc-text-input-warning': !!warningMessage,
          'oc-text-input-danger': !!errorMessage
        }"
        v-text="messageText"
      />
    </div>
    <portal-target v-if="type === 'password'" name="app.design-system.password-policy" />
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, useAttrs, useTemplateRef, unref } from 'vue'
import { uniqueId } from '../../helpers'
import OcButton from '../OcButton/OcButton.vue'
import OcIcon from '../OcIcon/OcIcon.vue'
import OcTextInputPassword from '../OcTextInputPassword/OcTextInputPassword.vue'
import { PasswordPolicy } from '../../helpers'

defineOptions({
  inheritAttrs: false
})

export interface Props {
  id?: string
  type?: 'text' | 'number' | 'email' | 'password'
  modelValue?: string
  selectionRange?: [number, number]
  clearButtonEnabled?: boolean
  clearButtonAccessibleLabel?: string
  defaultValue?: string
  disabled?: boolean
  label: string
  warningMessage?: string
  errorMessage?: string
  fixMessageLine?: boolean
  descriptionMessage?: string
  readOnly?: boolean
  passwordPolicy?: PasswordPolicy
  generatePasswordMethod?: (...args: unknown[]) => string
}

const {
  id = uniqueId('oc-textinput-'),
  type = 'text',
  modelValue = '',
  selectionRange,
  clearButtonEnabled = false,
  clearButtonAccessibleLabel = '',
  defaultValue,
  disabled = false,
  label,
  warningMessage,
  errorMessage,
  fixMessageLine = false,
  descriptionMessage,
  readOnly = false,
  passwordPolicy = {},
  generatePasswordMethod
} = defineProps<Props>()

const emit = defineEmits([
  'change',
  'update:modelValue',
  'focus',
  'passwordChallengeCompleted',
  'passwordChallengeFailed'
])

const showMessageLine = computed(() => {
  return fixMessageLine || !!warningMessage || !!errorMessage || !!descriptionMessage
})

const messageId = computed(() => `${id}-message`)

const additionalListeners = computed(() => {
  if (type === 'password') {
    return { passwordGenerated: onInput }
  }
  return {}
})

const tmpAttrs = useAttrs()
const additionalAttributes = computed(() => {
  const additionalAttrs: Record<string, unknown> = {}
  if (!!warningMessage || !!errorMessage || !!descriptionMessage) {
    additionalAttrs['aria-describedby'] = messageId.value
  }
  if (defaultValue) {
    additionalAttrs['placeholder'] = defaultValue
  }
  if (type === 'password') {
    additionalAttrs['password-policy'] = passwordPolicy
    additionalAttrs['generate-password-method'] = generatePasswordMethod
    additionalAttrs['has-warning'] = !!warningMessage
    additionalAttrs['has-error'] = !!errorMessage
  }
  // note: we spread out the attrs we don't want to be present in the resulting object
  const { change, input, focus, class: classes, ...attrs } = tmpAttrs
  return { ...attrs, ...additionalAttrs }
})

const ariaInvalid = computed(() => {
  return (!!errorMessage).toString()
})

const messageText = computed(() => {
  if (errorMessage) {
    return errorMessage
  }
  if (warningMessage) {
    return warningMessage
  }
  return descriptionMessage
})

const showClearButton = computed(() => {
  return !disabled && clearButtonEnabled && !!modelValue
})

const clearButtonAccessibleLabelValue = computed(() => {
  return clearButtonAccessibleLabel || 'Clear input'
})

const inputComponent = computed(() => {
  return type === 'password' ? OcTextInputPassword : 'input'
})

const inputRef = useTemplateRef<HTMLInputElement>('inputRef')
const focus = () => {
  unref(inputRef).focus()
}
defineExpose({ focus })

function onClear() {
  focus()
  onInput('')
  onChange(null)
}

const onChange = (value: string) => {
  emit('change', value)
}

const onInput = (value: string) => {
  emit('update:modelValue', value)
}

const onFocus = async (target: HTMLInputElement) => {
  await nextTick()
  target.select()
  if (selectionRange && selectionRange.length > 1) {
    target.setSelectionRange(selectionRange[0], selectionRange[1])
  }
  emit('focus', target.value)
}
</script>

<style lang="scss">
.oc-text-input-message.oc-text-input-description {
  display: flex;
  align-items: center;
  position: relative;
  padding-left: var(--oc-space-large);
  padding-top: calc(var(--oc-space-xsmall) - 2px);

  .oc-icon {
    position: absolute;
    left: var(--oc-space-xsmall);
    top: var(--oc-space-xsmall);
  }
}

.oc-text-input {
  &-description {
    color: var(--oc-color-text-muted);
  }

  &-success,
  &-success:focus {
    border-color: var(--oc-color-swatch-success-default) !important;
    color: var(--oc-color-swatch-success-default) !important;
  }

  &-warning,
  &-warning:focus {
    border-color: var(--oc-color-swatch-warning-default) !important;
    color: var(--oc-color-swatch-warning-default) !important;
  }

  &-danger,
  &-danger:focus {
    border-color: var(--oc-color-swatch-danger-default) !important;
    color: var(--oc-color-swatch-danger-default) !important;
  }

  &-message {
    display: flex;
    align-items: center;
    margin-top: var(--oc-space-xsmall);
    min-height: $oc-font-size-default * 1.5;
  }

  &.clear-action-visible {
    padding-right: ($oc-size-icon-default * 0.7) + 7px;
  }
}
</style>
