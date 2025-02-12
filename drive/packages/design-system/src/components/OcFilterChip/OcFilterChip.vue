<template>
  <div
    class="oc-filter-chip oc-flex"
    :class="{ 'oc-filter-chip-toggle': isToggle, 'oc-filter-chip-raw': raw }"
  >
    <oc-button
      :id="id"
      class="oc-filter-chip-button oc-pill"
      :class="{ 'oc-filter-chip-button-selected': filterActive }"
      appearance="raw"
      @click="isToggle ? emit('toggleFilter') : false"
    >
      <oc-icon
        :class="filterActive ? 'oc-filter-check-icon-active' : 'oc-filter-check-icon-inactive'"
        name="check"
        size="small"
        color="var(--oc-color-text-inverse)"
      />
      <span
        class="oc-text-truncate oc-filter-chip-label"
        v-text="!!selectedItemNames.length ? selectedItemNames[0] : filterLabel"
      />
      <span v-if="selectedItemNames.length > 1" v-text="` +${selectedItemNames.length - 1}`" />
      <oc-icon v-if="!filterActive && !isToggle" name="arrow-down-s" size="small" />
    </oc-button>
    <oc-drop
      v-if="!isToggle"
      ref="dropRef"
      :toggle="'#' + id"
      class="oc-filter-chip-drop"
      mode="click"
      padding-size="small"
      :close-on-click="closeOnClick"
      @hide-drop="emit('hideDrop')"
      @show-drop="emit('showDrop')"
    >
      <slot />
    </oc-drop>
    <oc-button
      v-if="filterActive"
      v-oc-tooltip="$gettext('Clear filter')"
      class="oc-filter-chip-clear oc-px-xs"
      appearance="raw"
      :aria-label="$gettext('Clear filter')"
      @click="emit('clearFilter')"
    >
      <oc-icon name="close" size="small" color="var(--oc-color-text-inverse)" />
    </oc-button>
  </div>
</template>

<script setup lang="ts">
import { computed, unref, useTemplateRef } from 'vue'
import { uniqueId } from '../../helpers'
import OcDrop from '../OcDrop/OcDrop.vue'

export interface Props {
  filterLabel: string
  closeOnClick?: boolean
  id?: string
  isToggle?: boolean
  isToggleActive?: boolean
  raw?: boolean
  selectedItemNames?: string[]
}

const {
  filterLabel,
  closeOnClick = false,
  id = uniqueId('oc-filter-chip-'),
  isToggle = false,
  isToggleActive = false,
  raw = false,
  selectedItemNames = []
} = defineProps<Props>()

const emit = defineEmits(['clearFilter', 'hideDrop', 'showDrop', 'toggleFilter'])

const dropRef = useTemplateRef<typeof OcDrop>('dropRef')

const filterActive = computed(() => {
  if (isToggle) {
    return isToggleActive
  }
  return !!selectedItemNames.length
})

const hideDrop = () => {
  unref(dropRef)?.hide()
}

defineExpose({ hideDrop })
</script>

<style lang="scss">
.oc-filter-chip {
  &-button.oc-pill {
    align-items: center;
    background-color: var(--oc-color-background-default) !important;
    color: var(--oc-color-text-muted) !important;
    border: 1px solid var(--oc-color-text-muted);
    box-sizing: border-box;
    display: inline-flex;
    gap: var(--oc-space-xsmall);
    text-decoration: none;
    font-size: var(--oc-font-size-xsmall);
    line-height: 1rem;
    max-width: 150px;
    padding: var(--oc-space-xsmall) var(--oc-space-small) !important;
    height: 100%;
  }
  &-button-selected.oc-pill,
  &-button-selected.oc-pill:hover {
    background-color: var(--oc-color-swatch-passive-default) !important;
    color: var(--oc-color-text-inverse) !important;
    border-top-left-radius: 99px !important;
    border-bottom-left-radius: 99px !important;
    border-top-right-radius: 0px !important;
    border-bottom-right-radius: 0px !important;
    border: 0;
  }
  &-clear,
  &-clear:hover {
    background-color: var(--oc-color-swatch-passive-default) !important;
    color: var(--oc-color-text-inverse) !important;
    border-top-left-radius: 0px !important;
    border-bottom-left-radius: 0px !important;
    border-top-right-radius: 99px !important;
    border-bottom-right-radius: 99px !important;
  }
  &-clear:not(.oc-filter-chip-toggle .oc-filter-chip-clear),
  &-clear:hover:not(.oc-filter-chip-toggle .oc-filter-chip-clear) {
    margin-left: 1px;
  }
}
.oc-filter-chip-raw {
  .oc-filter-chip-button {
    background-color: transparent !important;
    border: none !important;
  }
}
.oc-filter-check-icon-active {
  transition: all 0.25s ease-in;
  transform: scale(1) !important;
}
.oc-filter-check-icon-inactive {
  transition: all 0.25 ease-in;
  transform: scale(0) !important;
  width: 0px !important;
}

// the focussed button needs to stay above the other to correctly display the focus outline
.oc-filter-chip-button,
.oc-filter-chip-clear {
  &:focus {
    z-index: 9;
  }
}
</style>
