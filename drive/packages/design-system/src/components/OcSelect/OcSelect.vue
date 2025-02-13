<template>
  <div>
    <label v-if="!labelHidden" :aria-hidden="true" :for="id" class="oc-label" v-text="label" />
    <oc-contextual-helper
      v-if="contextualHelper?.isEnabled"
      v-bind="contextualHelper?.data"
      class="oc-pl-xs"
    />
    <vue-select
      ref="selectRef"
      :disabled="disabled || readOnly"
      :filter="filter"
      :loading="loading"
      :searchable="searchable"
      :clearable="clearable"
      :multiple="multiple"
      class="oc-select"
      :class="{ 'oc-select-position-fixed': positionFixed }"
      style="background: transparent"
      :dropdown-should-open="selectDropdownShouldOpen"
      :map-keydown="selectMapKeydown"
      v-bind="additionalAttributes"
      @update:model-value="emit('update:modelValue', $event)"
      @click="onSelectClick()"
      @search:blur="onSelectBlur()"
      @keydown="onSelectKeyDown($event)"
    >
      <template #search="{ attributes, events }">
        <input class="vs__search" v-bind="attributes" @input="userInput" v-on="events" />
      </template>
      <template v-for="(index, name) in $slots" #[name]="data">
        <slot v-if="name !== 'search'" :name="name" v-bind="data" />
      </template>
      <template #no-options>
        <div v-text="$gettext('No options available.')" />
      </template>
      <template #spinner="{ loading: loadingSpinner }">
        <oc-spinner v-if="loadingSpinner" />
      </template>
      <template #selected-option-container="{ option, deselect }">
        <span class="vs__selected" :class="{ 'vs__selected-readonly': option.readonly }">
          <slot name="selected-option" v-bind="option">
            <oc-icon v-if="readOnly" name="lock" class="oc-mr-xs" size="small" />
            {{ getOptionLabel(option) }}
          </slot>
          <span v-if="multiple" class="oc-flex oc-flex-middle oc-ml-s oc-mr-xs">
            <oc-icon v-if="option.readonly" class="vs__deselect-lock" name="lock" size="small" />
            <oc-button
              v-else
              appearance="raw"
              :title="$gettext('Deselect %{label}', { label: getOptionLabel(option) })"
              :aria-label="$gettext('Deselect %{label}', { label: getOptionLabel(option) })"
              class="vs__deselect oc-mx-rm"
              @mousedown.stop.prevent
              @click="deselect(option)"
            >
              <oc-icon name="close" size="small" />
            </oc-button>
          </span>
        </span>
      </template>
    </vue-select>

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
  </div>
</template>

<script setup lang="ts">
import Fuse from 'fuse.js'
import { uniqueId } from '../../helpers'
import {
  ref,
  unref,
  nextTick,
  watch,
  computed,
  onMounted,
  onBeforeUnmount,
  useAttrs,
  useTemplateRef
} from 'vue'
import { useGettext } from 'vue3-gettext'
import 'vue-select/dist/vue-select.css'
import { ContextualHelper } from '../../helpers'

export interface Props {
  id?: string
  filter?: (items: unknown[], search: string, { label }: { label?: string }) => unknown[]
  disabled?: boolean
  label: string
  labelHidden?: boolean
  contextualHelper?: ContextualHelper
  optionLabel?: string
  searchable?: boolean
  clearable?: boolean
  loading?: boolean
  warningMessage?: string
  errorMessage?: string
  fixMessageLine?: boolean
  descriptionMessage?: string
  multiple?: boolean
  readOnly?: boolean
  positionFixed?: boolean
}

// the keycode property is deprecated in the JS event API, vue-select still works with it though
enum KeyCode {
  Enter = 13,
  ArrowDown = 40,
  ArrowUp = 38
}

const {
  id = uniqueId('oc-select-'),
  filter = (items: unknown[], search: string, { label }: { label?: string }) => {
    if (items.length < 1) {
      return []
    }

    const fuse = new Fuse(items, {
      ...(label && { keys: [label] }),
      shouldSort: true,
      threshold: 0,
      ignoreLocation: true,
      distance: 100,
      minMatchCharLength: 1
    })

    return search.length ? fuse.search(search).map(({ item }) => item) : items
  },
  disabled = false,
  label,
  labelHidden = false,
  contextualHelper,
  optionLabel = 'label',
  searchable = true,
  clearable = false,
  loading = false,
  warningMessage,
  errorMessage,
  fixMessageLine = false,
  descriptionMessage,
  multiple = false,
  readOnly = false,
  positionFixed = false
} = defineProps<Props>()

const emit = defineEmits(['search:input', 'update:modelValue'])

const { $gettext } = useGettext()
const selectRef = useTemplateRef<typeof VueSelect>('selectRef')

const getOptionLabel = (option: string | Record<string, unknown>): string => {
  if (typeof option === 'object') {
    const key = optionLabel || label
    if (!Object.hasOwn(option, key)) {
      console.warn(
        `[vue-select warn]: Label key "option.${key}" does not` +
          ` exist in options object ${JSON.stringify(option)}.\n` +
          'https://vue-select.org/api/html#getoptionlabel'
      )
      return ''
    }
    return option[key] as string
  }
  return option
}

const setComboBoxAriaLabel = () => {
  const comboBoxElement = unref(selectRef).$el.querySelector('div:first-child')
  comboBoxElement?.setAttribute('aria-label', `${label} - ${$gettext('Search for option')}`)
}

const userInput = (event: Event) => {
  emit('search:input', (event.target as HTMLInputElement).value)
}

const dropdownEnabled = ref(false)
const setDropdownEnabled = (enabled: boolean) => {
  dropdownEnabled.value = enabled
}

const selectDropdownShouldOpen = ({
  noDrop,
  open,
  mutableLoading
}: {
  noDrop?: boolean
  open?: boolean
  mutableLoading?: boolean
}) => {
  return !noDrop && open && !mutableLoading && unref(dropdownEnabled)
}

const onSelectClick = () => {
  setDropdownEnabled(true)
}

const onSelectBlur = () => {
  setDropdownEnabled(false)
}

const setKeyboardOutline = async () => {
  const optionEls = (unref(selectRef).$refs.dropdownMenu as HTMLElement).querySelectorAll('li')
  const highlightedOption = optionEls[unref(selectRef).typeAheadPointer]
  if (highlightedOption) {
    await nextTick()
    highlightedOption.classList.add('keyboard-outline')
  }
}

const selectMapKeydown = (map: Record<number, (e: KeyboardEvent) => void>) => {
  return {
    ...map,
    [KeyCode.Enter]: (e: KeyboardEvent) => {
      if (!unref(dropdownEnabled)) {
        setDropdownEnabled(true)
        return
      }
      map[KeyCode.Enter](e)
      unref(selectRef).searchEl.focus()
    },
    [KeyCode.ArrowDown]: async (e: KeyboardEvent) => {
      e.preventDefault()
      unref(selectRef).typeAheadDown()

      if (unref(dropdownOpen)) {
        await setKeyboardOutline()
      }
    },
    [KeyCode.ArrowUp]: async (e: KeyboardEvent) => {
      e.preventDefault()
      unref(selectRef).typeAheadUp()

      if (unref(dropdownOpen)) {
        await setKeyboardOutline()
      }
    }
  }
}

const onSelectKeyDown = async (e: KeyboardEvent) => {
  if (e.key === 'Enter' || e.key === 'Tab') {
    if (unref(dropdownOpen)) {
      await setKeyboardOutline()
    }
    return
  }
  setDropdownEnabled(true)
}

const setDropdownPosition = () => {
  const dropdownMenu = unref(selectRef).$refs.dropdownMenu
  if (!dropdownMenu) {
    return
  }

  const toggleClientRect = unref(selectRef).$refs.toggle.getBoundingClientRect()
  const dropdownMenuBottomOffset = 25
  const dropdownMenuMaxHeight = Math.min(
    window.innerHeight - toggleClientRect.bottom - dropdownMenuBottomOffset,
    window.innerHeight
  )

  dropdownMenu.style.maxHeight = `${dropdownMenuMaxHeight}px`
  dropdownMenu.style.width = `${toggleClientRect.width}px`
  dropdownMenu.style.top = `${toggleClientRect.top + toggleClientRect.height + 1}px`
  dropdownMenu.style.left = `${toggleClientRect.left}px`
}

const dropdownOpen = computed(() => {
  return unref(selectRef)?.dropdownOpen
})

watch(dropdownOpen, async () => {
  if (positionFixed && unref(dropdownOpen)) {
    await nextTick()
    setDropdownPosition()
  }
})

onMounted(() => {
  setComboBoxAriaLabel()

  if (positionFixed) {
    window.addEventListener('resize', setDropdownPosition)
  }
})

onBeforeUnmount(() => {
  if (positionFixed) {
    window.removeEventListener('resize', setDropdownPosition)
  }
})

const attrs = useAttrs()
const additionalAttributes = computed(() => {
  const additionalAttrs: Record<string, unknown> = {}
  additionalAttrs['input-id'] = id
  additionalAttrs['getOptionLabel'] = getOptionLabel
  additionalAttrs['label'] = optionLabel

  return { ...attrs, ...additionalAttrs }
})

const showMessageLine = computed(() => {
  return fixMessageLine || !!warningMessage || !!errorMessage || !!descriptionMessage
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

const messageId = computed(() => {
  return `${id}-message`
})
</script>

<script lang="ts">
// @ts-ignore
import VueSelect from 'vue-select'

// importing VueSelect in script setup leads to an anomymousstub in unit tests
export default { components: { VueSelect } }
</script>

<style lang="scss">
.vs--disabled {
  cursor: not-allowed;

  .vs__clear,
  .vs__dropdown-toggle,
  .vs__open-indicator,
  .vs__search,
  .vs__selected {
    background-color: var(--oc-color-background-muted) !important;
    color: var(--oc-color-input-text-muted) !important;
    pointer-events: none;
  }

  .vs__actions {
    opacity: 0.3;
  }
}

.oc-select {
  line-height: normal;
  padding: 1px 0;
  color: var(--oc-color-input-text-default);

  &-position-fixed {
    .vs__dropdown-menu {
      position: fixed;
      overflow-y: auto;
    }
  }

  .vs {
    &__search {
      color: var(--oc-color-input-text-default);
    }

    &__search::placeholder,
    &__dropdown-toggle,
    &__dropdown-menu {
      -webkit-appearance: none;
      background-color: var(--oc-color-background-default);
      border-radius: 0;
      border-radius: 5px;
      border: 1px solid var(--oc-color-input-border);
      box-sizing: border-box;
      color: var(--oc-color-input-text-default);
      line-height: inherit;
      margin: 0;
      max-width: 100%;
      outline: none;
      padding: 2px;
      transition-duration: 0.2s;
      transition-timing-function: ease-in-out;
      transition-property: color, background-color;
      width: 100%;
    }

    &__selected-readonly {
      background-color: var(--oc-color-background-muted) !important;
    }

    &__search,
    &__search:focus {
      padding: 0 5px;
    }

    &__dropdown-menu {
      padding: 0;
      background-color: var(--oc-color-background-default);
      margin-top: -1px;
    }

    &__clear,
    &__open-indicator,
    &__deselect {
      fill: var(--oc-color-input-text-default);
    }

    &__deselect {
      margin: 0 var(--oc-space-small);
    }

    &__dropdown-option,
    &__no-options {
      color: var(--oc-color-input-text-default);
      white-space: normal;
      padding: 6px 0.6rem;
      border-radius: 5px;
      line-height: var(--vs-line-height);

      &--highlight {
        background-color: var(--oc-color-background-hover);
        color: var(--oc-color-swatch-brand-contrast);
      }

      &--selected {
        background-color: var(--oc-color-background-highlight);
        color: var(--oc-color-swatch-brand-contrast);
      }
    }

    &__actions {
      flex-flow: row wrap;
      gap: var(--oc-space-xsmall);
      cursor: pointer;

      svg {
        overflow: visible;
      }
    }

    &__clear svg {
      max-width: var(--oc-space-small);
    }

    &__selected-options {
      flex: auto;
      padding: 0;

      > * {
        padding: 0px 2px;
        margin: 2px 2px 2px 1px;
        color: var(--oc-color-input-text-default);
      }

      > *:not(input) {
        padding-left: 3px;
        background-color: var(--oc-color-background-default);
        fill: var(--oc-color-text-default);
      }
    }
  }

  &.vs--multiple {
    .vs {
      &__selected-options > *:not(input) {
        color: var(--oc-color-input-text-default);
        background-color: var(--oc-color-background-default);
      }
    }
  }

  &:focus-within {
    .vs__dropdown-menu,
    .vs__dropdown-toggle {
      border-color: var(--oc-color-swatch-passive-default);
    }
  }

  .keyboard-outline {
    outline: 2px var(--oc-color-swatch-passive-default) solid !important;
    outline-offset: -2px;
  }
}

.oc-background-highlight {
  .oc-select {
    .vs {
      &__search {
        color: var(--oc-color-input-text-default);
      }

      &__search::placeholder,
      &__dropdown-toggle,
      &__dropdown-menu {
        background-color: var(--oc-color-input-bg);
      }
    }

    &.vs--multiple {
      .vs__selected-options > *:not(input) {
        color: var(--oc-color-input-text-default);
        background-color: var(--oc-color-background-default);
      }
    }

    &:focus-within {
      .vs__dropdown-menu,
      .vs__dropdown-toggle {
        background-color: var(--oc-color-background-default);
      }
    }
  }
}

.vs--single {
  &.vs--open .vs__selected {
    opacity: 0.8 !important;
  }

  .vs__selected-options > *:not(input) {
    background-color: transparent !important;
  }
}
</style>
