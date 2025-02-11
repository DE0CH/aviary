<template>
  <oc-grid
    flex
    :role="isFilter ? undefined : 'search'"
    class="oc-search oc-flex-middle"
    :class="{ 'oc-search-small': small }"
  >
    <div class="oc-width-expand oc-position-relative">
      <input
        v-model="model"
        :class="inputClass"
        :aria-label="label"
        :disabled="loading"
        :placeholder="placeholder"
        @keydown.enter="onSearch"
        @keyup="$emit('keyup', $event)"
      />
      <slot name="locationFilter" />
      <oc-button
        v-if="icon"
        v-oc-tooltip="$gettext('Search')"
        :aria-label="$gettext('Search')"
        class="oc-position-small oc-position-center-right oc-mt-rm"
        appearance="raw"
        @click.prevent.stop="$emit('advancedSearch', $event)"
      >
        <oc-icon v-show="!loading" :name="icon" size="small" fill-type="line" variation="passive" />
        <oc-spinner
          v-show="loading"
          :size="small ? 'xsmall' : 'medium'"
          :aria-label="loadingAccessibleLabelValue"
        />
      </oc-button>
    </div>
    <div class="oc-search-button-wrapper" :class="{ 'oc-invisible-sr': buttonHidden }">
      <oc-button
        class="oc-search-button"
        variation="primary"
        appearance="filled"
        :size="small ? 'small' : 'medium'"
        :disabled="loading || model.length < 1"
        @click="onSearch"
      >
        {{ buttonLabel }}
      </oc-button>
    </div>
    <oc-button
      v-if="showCancelButton"
      :variation="cancelButtonVariation"
      :appearance="cancelButtonAppearance"
      class="oc-ml-m"
      @click="onCancel"
    >
      <span v-text="$gettext('Cancel')" />
    </oc-button>
  </oc-grid>
</template>

<script lang="ts" setup>
import { computed, unref, useSlots, watch } from 'vue'
import { useGettext } from 'vue3-gettext'
import OcButton from '../OcButton/OcButton.vue'
import OcGrid from '../OcGrid/OcGrid.vue'
import OcIcon from '../OcIcon/OcIcon.vue'
import OcSpinner from '../OcSpinner/OcSpinner.vue'

export interface Props {
  icon?: string
  placeholder?: string
  label: string
  small?: boolean
  buttonLabel?: string
  buttonHidden?: boolean
  typeAhead?: boolean
  trimQuery?: boolean
  loading?: boolean
  isFilter?: boolean
  loadingAccessibleLabel?: string
  showCancelButton?: boolean
  cancelButtonVariation?: 'passive' | 'primary' | 'danger' | 'success' | 'warning' | 'brand'
  cancelButtonAppearance?: 'filled' | 'outline' | 'raw' | 'raw-inverse'
  cancelHandler?: () => void
}

const {
  icon = 'search',
  placeholder = '',
  label,
  small = false,
  buttonLabel = 'Search',
  buttonHidden = false,
  typeAhead = false,
  trimQuery = true,
  loading = false,
  isFilter = false,
  loadingAccessibleLabel = '',
  showCancelButton = false,
  cancelButtonVariation = 'primary',
  cancelButtonAppearance = 'raw',
  cancelHandler = () => {}
} = defineProps<Props>()

const model = defineModel<string>({ default: '' })
watch(model, () => {
  if (typeAhead) {
    onSearch()
  }
})

const emit = defineEmits(['advancedSearch', 'clear', 'keyup', 'search'])

const { $gettext } = useGettext()
const slots = useSlots()

const inputIconRightPadding = computed(() => {
  if (Object.hasOwn(slots, 'locationFilter')) {
    return '125px'
  }
  return '48px'
})

const inputClass = computed(() => {
  const classes = ['oc-search-input', 'oc-input']
  if (!buttonHidden) {
    classes.push('oc-search-input-button')
  }
  return classes
})

const loadingAccessibleLabelValue = computed(() => {
  return loadingAccessibleLabel || 'Loading results'
})

const onSearch = () => {
  emit('search', trimQuery ? unref(model).trim() : unref(model))
}

const onCancel = () => {
  model.value = ''
  if (!typeAhead) {
    onSearch()
  }
  cancelHandler()
}
</script>

<style lang="scss">
.oc-search {
  min-width: $form-width-medium;

  &-button {
    border-bottom-left-radius: 0;
    border-top-left-radius: 0;
    // Prevent double borders
    // from input and button
    transform: translateX(-1px);
    z-index: 0;
  }

  &-icon {
    align-items: center;
    bottom: 0;
    color: var(--oc-color-text-muted);
    display: inline-flex;
    justify-content: center;
    left: 0;
    position: absolute;
    top: 0;
    width: 40px;
  }

  &-input {
    border-radius: 25px !important;
    border: none;
    padding: var(--oc-space-medium) !important;

    &:focus {
      background-color: var(--oc-color-input-bg);
      border-color: var(--oc-color-input-text-default);
      color: var(--oc-color-input-text-default);
      background-image: none;
    }

    &::-ms-clear,
    &::-ms-reveal {
      display: none;
    }
  }

  &-input-icon {
    padding-left: var(--oc-space-xlarge) !important;
    padding-right: v-bind(inputIconRightPadding) !important;
  }

  &-input-button {
    border-bottom-right-radius: 0;
    border-top-right-radius: 0;
  }

  &-clear {
    right: var(--oc-space-large) !important;
  }

  &-small {
    .oc-search-input {
      height: 30px;
      line-height: 28px;
      padding-left: var(--oc-space-xlarge);
    }

    .oc-icon {
      &,
      svg {
        height: 18px;
        width: 18px;
      }
    }
  }
}
</style>
