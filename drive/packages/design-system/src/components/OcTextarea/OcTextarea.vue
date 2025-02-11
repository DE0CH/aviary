<template>
  <div>
    <label class="oc-label" :for="id" v-text="label" />
    <textarea
      :id="id"
      v-bind="additionalAttributes"
      ref="textareaRef"
      v-model="model"
      class="oc-textarea oc-rounded"
      :class="{
        'oc-textarea-warning': !!warningMessage,
        'oc-textarea-danger': !!errorMessage
      }"
      :aria-invalid="ariaInvalid"
      @keydown="onKeyDown($event)"
    />
    <div v-if="showMessageLine" class="oc-textarea-message">
      <span
        :id="messageId"
        :class="{
          'oc-textarea-description': !!descriptionMessage,
          'oc-textarea-warning': !!warningMessage,
          'oc-textarea-danger': !!errorMessage
        }"
        v-text="messageText"
      />
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, HTMLAttributes, unref, useAttrs, useTemplateRef } from 'vue'
import { uniqueId } from '../../helpers'

export interface Props {
  id?: string
  label: string
  warningMessage?: string
  errorMessage?: string
  descriptionMessage?: string
  fixMessageLine?: boolean
  submitOnEnter?: boolean
}

const {
  id = uniqueId('oc-textarea-'),
  label,
  warningMessage,
  errorMessage,
  descriptionMessage,
  fixMessageLine = false,
  submitOnEnter = false
} = defineProps<Props>()

const emit = defineEmits(['change', 'keydown'])

const model = defineModel<string>({ default: '' })

const showMessageLine = computed(() => {
  return fixMessageLine || !!warningMessage || !!errorMessage || !!descriptionMessage
})

const messageId = computed(() => `${id}-message`)

const attrs = useAttrs()
const additionalAttributes = computed(() => {
  const additionalAttrs: Record<string, unknown> = {}
  if (!!warningMessage || !!errorMessage || !!descriptionMessage) {
    additionalAttrs['aria-describedby'] = messageId.value
  }
  return { ...attrs, ...additionalAttrs }
})

const ariaInvalid = computed(() => {
  return (!!errorMessage).toString() as HTMLAttributes['aria-invalid']
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

const textareaRef = useTemplateRef<HTMLInputElement>('textareaRef')
const focus = () => {
  unref(textareaRef).focus()
}
defineExpose({ focus })

const onKeyDown = (e: KeyboardEvent) => {
  const enterKey = e.key?.toLowerCase() === 'enter'
  if (submitOnEnter && enterKey && !e.ctrlKey && !e.shiftKey) {
    emit('change', (e.target as HTMLInputElement).value)
  }
  emit('keydown', e)
}
</script>

<style lang="scss">
.oc-textarea {
  padding-bottom: var(--oc-space-xsmall);
  padding-top: var(--oc-space-xsmall);
  box-sizing: border-box;
  background: var(--oc-color-background-muted);
  border: 0 none;
  margin: 0;
  color: var(--oc-color-text-default);
  max-width: 100%;
  width: 100%;
  overflow: auto;

  &:disabled {
    color: var(--oc-color-input-text-muted);
  }

  &:focus {
    border-color: var(--oc-color-input-text-default);
    background-color: var(--oc-color-background-muted);
    color: var(--oc-color-text-default);
  }

  &-warning,
  &-warning:focus {
    border-color: var(--oc-color-swatch-warning-default);
    color: var(--oc-color-swatch-warning-default);
  }

  &-danger,
  &-danger:focus {
    border-color: var(--oc-color-swatch-danger-default);
    color: var(--oc-color-swatch-danger-default);
  }

  &-description {
    color: var(--oc-color-text-muted);
  }

  &-message {
    display: flex;
    align-items: center;
    margin-top: var(--oc-space-xsmall);

    min-height: $oc-font-size-default * 1.5;
  }
}
</style>
