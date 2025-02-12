<template>
  <div class="oc-modal-background">
    <focus-trap :active="true" :initial-focus="initialFocusRef" :tabbable-options="tabbableOptions">
      <div
        :id="elementId"
        ref="ocModal"
        :class="classes"
        tabindex="0"
        role="dialog"
        aria-modal="true"
        aria-labelledby="oc-modal-title"
        @keydown.esc="cancelModalAction"
      >
        <div class="oc-modal-title">
          <oc-icon v-if="iconName !== ''" :name="iconName" :variation="variation" />
          <h2 id="oc-modal-title" class="oc-text-truncate" v-text="title" />
        </div>
        <div class="oc-modal-body">
          <div v-if="$slots.content" key="modal-slot-content" class="oc-modal-body-message">
            <slot name="content" />
          </div>
          <template v-else>
            <p
              v-if="message"
              key="modal-message"
              class="oc-modal-body-message oc-mt-rm"
              :class="{ 'oc-mb-rm': !hasInput || contextualHelperData }"
              v-text="message"
            />
            <div
              v-if="contextualHelperData"
              class="oc-modal-body-contextual-helper"
              :class="{ 'oc-mb-rm': !hasInput }"
            >
              <span class="text" v-text="contextualHelperLabel" />
              <oc-contextual-helper class="oc-pl-xs" v-bind="contextualHelperData" />
            </div>
            <oc-text-input
              v-if="hasInput"
              key="modal-input"
              ref="ocModalInput"
              v-model="userInputValue"
              class="oc-modal-body-input"
              :error-message="inputError"
              :label="inputLabel"
              :type="inputType"
              :description-message="inputDescription"
              :fix-message-line="true"
              :selection-range="inputSelectionRange"
              @update:model-value="inputOnInput"
              @keydown.enter.prevent="confirm"
            />
          </template>
        </div>

        <div v-if="!hideActions" class="oc-modal-body-actions oc-flex oc-flex-right">
          <div class="oc-modal-body-actions-grid">
            <oc-button
              class="oc-modal-body-actions-cancel"
              variation="passive"
              appearance="outline"
              :disabled="isLoading"
              @click="cancelModalAction"
              >{{ $gettext(buttonCancelText) }}
            </oc-button>
            <oc-button
              v-if="!hideConfirmButton"
              class="oc-modal-body-actions-confirm oc-ml-s"
              variation="primary"
              :appearance="buttonConfirmAppearance"
              :disabled="isLoading || buttonConfirmDisabled || !!inputError"
              :show-spinner="showSpinner"
              @click="confirm"
              >{{ $gettext(buttonConfirmText) }}
            </oc-button>
          </div>
        </div>
      </div>
    </focus-trap>
  </div>
</template>

<script setup lang="ts">
import { ref, watch, computed, unref, useTemplateRef } from 'vue'
import OcButton, { Props as ButtonProps } from '../OcButton/OcButton.vue'
import OcIcon from '../OcIcon/OcIcon.vue'
import OcTextInput from '../OcTextInput/OcTextInput.vue'
import { FocusTargetOrFalse, FocusTrapTabbableOptions } from 'focus-trap'
import { ContextualHelperData } from '../../helpers'
import { useGettext } from 'vue3-gettext'

export interface Props {
  title: string
  buttonCancelText?: string
  buttonConfirmDisabled?: boolean
  buttonConfirmText?: string
  contextualHelperData?: ContextualHelperData
  contextualHelperLabel?: string
  elementClass?: string
  elementId?: string
  focusTrapInitial?: string | boolean
  hasInput?: boolean
  hideActions?: boolean
  hideConfirmButton?: boolean
  icon?: string
  inputDescription?: string
  inputError?: string
  inputLabel?: string
  inputSelectionRange?: [number, number]
  inputType?: 'text' | 'number' | 'email' | 'password'
  inputValue?: string
  isLoading?: boolean
  message?: string
  variation?:
    | 'passive'
    | 'primary'
    | 'danger'
    | 'success'
    | 'warning'
    | 'info'
    | 'brand'
    | 'inherit'
}

const {
  title,
  buttonCancelText = 'Cancel',
  buttonConfirmDisabled = false,
  buttonConfirmText = 'Confirm',
  contextualHelperData,
  contextualHelperLabel = '',
  elementClass,
  elementId,
  focusTrapInitial = null,
  hasInput = false,
  hideActions = false,
  hideConfirmButton = false,
  icon,
  inputDescription,
  inputError,
  inputLabel,
  inputSelectionRange,
  inputType = 'text',
  inputValue,
  isLoading = false,
  message,
  variation = 'passive'
} = defineProps<Props>()

const emit = defineEmits(['cancel', 'confirm', 'input'])

const { $gettext } = useGettext()

const showSpinner = ref(false)
const userInputValue = ref<string>()
const buttonConfirmAppearance = ref<ButtonProps['appearance']>('filled')
const ocModal = useTemplateRef<HTMLElement>('ocModal')
const ocModalInput = useTemplateRef<typeof OcTextInput>('ocModalInput')

const tabbableOptions = computed((): FocusTrapTabbableOptions => {
  return {
    getShadowRoot: true
  }
})

const resetLoadingState = () => {
  showSpinner.value = false
  buttonConfirmAppearance.value = 'filled'
}

const setLoadingState = () => {
  showSpinner.value = true
  buttonConfirmAppearance.value = 'outline'
}

watch(
  () => isLoading,
  () => {
    if (!isLoading) {
      return resetLoadingState()
    }
    setTimeout(() => {
      if (!isLoading) {
        return resetLoadingState()
      }
      setLoadingState()
    }, 700)
  },
  { immediate: true }
)

const initialFocusRef = computed<FocusTargetOrFalse>(() => {
  if (focusTrapInitial || focusTrapInitial === false) {
    return focusTrapInitial as FocusTargetOrFalse
  }
  // needs to be one of those elements or undefined. null will throw errors
  return () => unref(ocModalInput)?.$el || unref(ocModal) || undefined
})

const classes = computed(() => {
  return ['oc-modal', `oc-modal-${variation}`, elementClass]
})

const iconName = computed(() => {
  if (icon) {
    return icon
  }
  switch (variation) {
    case 'danger':
      return 'alert'
    case 'warning':
      return 'error-warning'
    case 'success':
      return 'checkbox-circle'
    case 'info':
      return 'information'
    default:
      return ''
  }
})

watch(
  () => inputValue,
  (value) => {
    userInputValue.value = value
  },
  { immediate: true }
)

const cancelModalAction = () => {
  emit('cancel')
}

const confirm = () => {
  if (buttonConfirmDisabled || inputError) {
    return
  }
  emit('confirm', unref(userInputValue))
}

const inputOnInput = (value: string) => {
  emit('input', value)
}
</script>

<script lang="ts">
// this needs to be non-script-setup so we can use FocusTrap in unit tests
import { FocusTrap } from 'focus-trap-vue'

export default {
  components: { FocusTrap }
}
</script>

<style lang="scss">
@mixin oc-modal-variation($color) {
  span {
    color: $color;
  }
}

.oc-modal {
  background-color: var(--oc-color-background-default);
  border-radius: 5px;
  box-shadow: 5px 0 25px rgba(0, 0, 0, 0.3);
  max-height: 90vh;
  max-width: 500px;
  overflow: auto;
  width: 100%;

  &:focus {
    outline: none;
  }

  &-background {
    align-items: center;
    background-color: rgba(0, 0, 0, 0.4);
    display: flex;
    flex-flow: row wrap;
    height: 100%;
    justify-content: center;
    left: 0;
    position: fixed;
    top: 0;
    width: 100%;
    z-index: var(--oc-z-index-modal);
  }

  &-primary {
    @include oc-modal-variation(var(--oc-color-swatch-primary-default));
  }

  &-success {
    @include oc-modal-variation(var(--oc-color-swatch-success-default));
  }

  &-warning {
    @include oc-modal-variation(var(--oc-color-swatch-warning-default));
  }

  &-danger {
    @include oc-modal-variation(var(--oc-color-swatch-danger-default));
  }

  &-title {
    align-items: center;
    border-top-left-radius: 5px;
    border-top-right-radius: 5px;
    display: flex;
    flex-flow: row wrap;
    line-height: 1.625;
    padding: calc(var(--oc-space-small) + var(--oc-space-xsmall)) var(--oc-space-medium);
    background-color: var(--oc-color-swatch-inverse-muted);

    > .oc-icon {
      margin-right: var(--oc-space-small);
    }

    > h2 {
      font-size: 1rem;
      font-weight: bold;
      margin: 0;
    }
  }

  &-body {
    color: var(--oc-color-text-default);
    line-height: 1.625;
    padding: var(--oc-space-medium) var(--oc-space-medium) 0;

    &-message {
      margin-bottom: var(--oc-space-medium);
      margin-top: var(--oc-space-small);
    }

    &-contextual-helper {
      margin-bottom: var(--oc-space-medium);
    }

    .oc-input {
      line-height: normal;
    }

    &-input {
      /* FIXME: this is ugly, but required so that the bottom padding doesn't look off when reserving vertical space for error messages below the input. */
      margin-bottom: -20px;
      padding-bottom: var(--oc-space-medium);

      .oc-text-input-message {
        margin-bottom: var(--oc-space-xsmall);
      }
    }

    &-actions {
      text-align: right;
      background: var(--oc-color-background-default);
      border-bottom-right-radius: 15px;
      border-bottom-left-radius: 15px;
      padding: var(--oc-space-medium);

      .oc-button {
        border-radius: 4px;
      }

      &-grid {
        display: inline-grid;
        grid-auto-flow: column;
        grid-auto-columns: 1fr;
      }
    }
  }
}
</style>
