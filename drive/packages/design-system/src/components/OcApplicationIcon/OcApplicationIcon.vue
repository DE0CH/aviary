<template>
  <div class="oc-application-icon oc-flex-inline oc-flex-middle oc-flex-center" :style="iconStyle">
    <oc-icon :name="icon" :color="iconColor" size="medium" />
  </div>
</template>

<script setup lang="ts">
import {
  generateHashedColorForString,
  getHexFromCssVar,
  hexToRgb,
  rgbToHex,
  setDesiredContrastRatio,
  calculateShadeColor
} from '../../helpers'
import { computed, unref } from 'vue'
import OcIcon from '../OcIcon/OcIcon.vue'

export interface Props {
  icon: string
  colorPrimary?: string
  colorSecondary?: string
}
const { icon, colorPrimary, colorSecondary } = defineProps<Props>()

const iconColor = computed(() => {
  return 'rgba(255,255,255,0.7)'
})

const getGradient = (primary: string, secondary: string) => {
  return `linear-gradient(90deg, ${primary} 0%, ${secondary} 100%)`
}

const primaryColor = computed(() => {
  return getHexFromCssVar(colorPrimary || '')
})

const secondaryColor = computed(() => {
  return getHexFromCssVar(colorSecondary || '')
})

const hasPrimaryColor = computed(() => {
  return !!colorPrimary
})

const hasSecondaryColor = computed(() => {
  return !!colorSecondary
})

const generatedHashedPrimaryColor = computed((): string => {
  const hashedColor = generateHashedColorForString(icon)
  return rgbToHex(setDesiredContrastRatio(hexToRgb(hashedColor), hexToRgb('#ffffff'), 4))
})

const iconStyle = computed(() => {
  const primaryHex = unref(hasPrimaryColor)
    ? unref(primaryColor)
    : unref(generatedHashedPrimaryColor)
  const secondaryHex = unref(hasSecondaryColor)
    ? unref(secondaryColor)
    : calculateShadeColor(hexToRgb(primaryHex), 40)

  const darkBorderHex = calculateShadeColor(hexToRgb(primaryHex), -25)
  const lightBorderHex = calculateShadeColor(hexToRgb(primaryHex), 45)
  return {
    background: getGradient(primaryHex, secondaryHex),
    boxShadow: `inset ${lightBorderHex} 0px 0px 1px 0px,${darkBorderHex} 0px 0px 1px 0px`
  }
})
</script>

<style lang="scss">
.oc-application-icon {
  width: 28px;
  height: 28px;
  border-radius: 4px;

  .oc-icon {
    height: 18px !important;
    max-height: 18px !important;
    max-width: 18px !important;
    width: 18px !important;

    svg {
      fill: var(--oc-color-swatch-primary-contrast) !important;
      height: 18px !important;
      max-height: 18px !important;
      max-width: 18px !important;
      width: 18px !important;
    }
  }
}
</style>
