<template>
  <component
    :is="type"
    v-bind="additionalAttributes"
    :aria-label="ariaLabel"
    :class="buttonClass"
    v-on="handlers"
  >
    <oc-spinner v-if="showSpinner" size="small" class="spinner" />
    <!-- @slot Content of the button -->
    <slot />
  </component>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { RouteLocationRaw } from 'vue-router'
import { getSizeClass } from '../../helpers'

export interface Props {
  /**
   * @docs The appearance of the button.
   * @default outline
   */
  appearance?: 'filled' | 'outline' | 'raw' | 'raw-inverse'
  /**
   * @docs The aria label of the button. Needs to be present if the button doesn't have a visible label.
   */
  ariaLabel?: string
  /**
   * @docs Determines if the button is disabled.
   * @default false
   */
  disabled?: boolean
  /**
   * @docs The gap size between content elements of the button.
   * @default medium
   */
  gapSize?: 'none' | 'xsmall' | 'small' | 'medium' | 'large' | 'xlarge'
  /**
   * @docs The href if the `type` is set to `a'.
   */
  href?: string
  /**
   * @docs The alignment of the button content.
   * @default center
   */
  justifyContent?: 'left' | 'center' | 'right' | 'space-around' | 'space-between' | 'space-evenly'
  /**
   * @docs Determines if a spinner should be shown inside the button.
   * @default false
   */
  showSpinner?: boolean
  /**
   * @docs The size of the button.
   * @default medium
   */
  size?: 'small' | 'medium' | 'large'
  /**
   * @docs The type of the button element. Only takes effect if the `type` is set to `button`.
   * @default button
   */
  submit?: 'null' | 'button' | 'submit' | 'reset'
  /**
   * @docs The target of the button if the `type` is set to `a`.
   */
  target?: '_blank' | '_self' | '_parent' | '_top'
  /**
   * @docs The route location if the `type` is set to `router-link`.
   */
  to?: RouteLocationRaw
  /**
   * @docs The type of the button element.
   * @default button
   */
  type?: 'button' | 'a' | 'router-link'
  /**
   * @docs The variation of the button.
   * @default passive
   */
  variation?: 'passive' | 'primary' | 'danger' | 'success' | 'warning' | 'brand'
}

export interface Emits {
  /**
   * @docs Emitted when the button has been clicked.
   */
  (e: 'click', event: MouseEvent): void
}

export interface Slots {
  /**
   * @docs Button content.
   */
  default?: () => unknown
}

const {
  appearance = 'outline',
  ariaLabel,
  disabled = false,
  gapSize = 'medium',
  href,
  justifyContent = 'center',
  showSpinner = false,
  size = 'medium',
  submit = 'button',
  target,
  to,
  type = 'button',
  variation = 'passive'
} = defineProps<Props>()

const emit = defineEmits<Emits>()
defineSlots<Slots>()

const buttonClass = computed(() => {
  return [
    'oc-button',
    'oc-rounded',
    `oc-button-${getSizeClass(size)}`,
    `oc-button-justify-content-${justifyContent}`,
    `oc-button-gap-${getSizeClass(gapSize)}`,
    `oc-button-${variation}`,
    `oc-button-${appearance}`,
    `oc-button-${variation}-${appearance}`
  ]
})

const additionalAttributes = computed(() => {
  return {
    ...(href && { href }),
    ...(target && { target }),
    ...(to && { to }),
    ...(type === 'button' && { type: submit }),
    ...(type === 'button' && { disabled })
  }
})

const handlers = computed(() => {
  return {
    ...(type === 'button' && { click: onClick })
  }
})

const onClick = (event: MouseEvent) => {
  emit('click', event)
}
</script>

<style lang="scss">
@mixin oc-button-gap($factor) {
  gap: math.round(calc($oc-space-small * $factor / 2)) * 2;
}

@mixin oc-button-line-height($factor) {
  line-height: $oc-size-icon-default * $factor;
}

@mixin oc-button-variation($color, $hover-color, $muted-color, $contrast-color) {
  &:disabled {
    background-color: $muted-color;
  }

  &:focus:not([disabled]),
  &:hover:not([disabled]) {
    background-color: $hover-color;
  }

  &-raw,
  &-raw-inverse {
    background-color: transparent;
    border-style: none;
    font-size: var(--oc-font-size-medium);
    font-weight: normal;
    min-height: 0;
    padding: 0;

    &:focus:not([disabled]),
    &:hover:not([disabled]) {
      background-color: transparent;
    }

    &:focus:not([disabled]):not(button),
    &:hover:not([disabled]):not(button) {
      text-decoration: underline;
    }

    &:disabled {
      background-color: transparent;
      color: $muted-color;
    }
  }
  &-raw {
    color: $color;

    .oc-icon > svg {
      fill: $color;
    }
  }
  &-raw-inverse {
    color: $contrast-color;

    .oc-icon > svg {
      fill: $contrast-color;
    }
  }

  &-filled {
    background-color: $color;
    color: $contrast-color;

    .oc-icon > svg {
      fill: $contrast-color;
    }

    &:hover:not([disabled]),
    &:focus:not([disabled]) {
      color: $contrast-color;
      background-color: $hover-color;
      border-color: $hover-color;

      .oc-icon > svg {
        fill: $contrast-color;
      }
    }
  }

  &-outline {
    outline: 1px solid $color;
    outline-offset: -1px;
    background-color: transparent;
    color: $color;

    .oc-icon > svg {
      fill: $color;
    }

    &:disabled {
      background-color: transparent;
      color: $muted-color;
    }
  }

  &-outline:hover:not([disabled]),
  &-outline:focus:not([disabled]) {
    color: $contrast-color;
    background-color: $hover-color;
    border-color: $hover-color;

    .oc-icon > svg {
      fill: $contrast-color;
    }
  }
}

.oc-button {
  @include oc-button-line-height(1);

  align-items: center;
  border: 0;
  box-sizing: border-box;
  display: inline-flex;
  font-weight: 400;
  padding: 0.5rem 0.8rem;
  text-align: left;
  text-decoration: none;

  &-justify-content {
    &-left {
      justify-content: left;
    }

    &-center {
      justify-content: center;
    }

    &-right {
      justify-content: right;
    }

    &-space-between {
      justify-content: space-between;
    }

    &-space-around {
      justify-content: space-around;
    }

    &-space-evenly {
      justify-content: space-evenly;
    }
  }

  &-gap {
    &-xs {
      @include oc-button-gap(0.5);
    }

    &-s {
      @include oc-button-gap(0.7);
    }

    &-m {
      @include oc-button-gap(1);
    }

    &-l {
      @include oc-button-gap(1.5);
    }

    &-xl {
      @include oc-button-gap(2);
    }
  }

  &:hover {
    cursor: pointer;
  }

  &-s {
    @include oc-button-line-height(0.7);

    font-size: var(--oc-font-size-small);
    min-height: 1.2rem;
  }

  &-m {
    @include oc-button-line-height(1);

    font-size: var(--oc-font-size-medium);
    min-height: $global-control-height;
  }

  &-l {
    @include oc-button-line-height(1.5);

    font-size: var(--oc-font-size-xlarge);
    min-height: 2rem;
  }

  &-passive {
    @include oc-button-variation(
      var(--oc-color-swatch-passive-default),
      var(--oc-color-swatch-passive-hover),
      var(--oc-color-swatch-passive-muted),
      var(--oc-color-swatch-passive-contrast)
    );

    &-outline {
      &:focus:not([disabled]),
      &:hover:not([disabled]) {
        color: var(--oc-color-swatch-passive-default);
        background-color: var(--oc-color-swatch-passive-hover-outline);
        border-color: var(--oc-color-swatch-passive-hover-outline);

        .oc-icon > svg {
          fill: var(--oc-color-swatch-passive-default);
        }
      }
    }
  }

  &-brand {
    @include oc-button-variation(
      var(--oc-color-swatch-brand-default),
      var(--oc-color-swatch-brand-hover),
      var(--oc-color-swatch-brand-muted),
      var(--oc-color-swatch-brand-contrast)
    );
  }

  &-primary {
    @include oc-button-variation(
      var(--oc-color-swatch-primary-default),
      var(--oc-color-swatch-primary-hover),
      var(--oc-color-swatch-primary-muted),
      var(--oc-color-swatch-primary-contrast)
    );

    &-filled {
      color: var(--oc-color-swatch-primary-contrast) !important;

      span > svg {
        fill: var(--oc-color-swatch-primary-contrast) !important;
      }
    }
  }

  &-success {
    @include oc-button-variation(
      var(--oc-color-swatch-success-default),
      var(--oc-color-swatch-success-hover),
      var(--oc-color-swatch-success-muted),
      var(--oc-color-swatch-success-contrast)
    );
  }

  &-warning {
    @include oc-button-variation(
      var(--oc-color-swatch-warning-default),
      var(--oc-color-swatch-warning-hover),
      var(--oc-color-swatch-warning-muted),
      var(--oc-color-swatch-warning-contrast)
    );
  }

  &-danger {
    @include oc-button-variation(
      var(--oc-color-swatch-danger-default),
      var(--oc-color-swatch-danger-hover),
      var(--oc-color-swatch-danger-muted),
      var(--oc-color-swatch-danger-contrast)
    );
  }

  &:disabled {
    cursor: default;
    opacity: 0.6;
  }

  &-group {
    display: inline-flex;
    flex-flow: row wrap;
    outline: 1px solid var(--oc-color-swatch-passive-default);
    outline-offset: -1px;
    border-radius: 5px;

    .oc-button {
      border-radius: 0;
      outline: 0;

      &:first-of-type {
        border-radius: 5px 0 0 5px;
      }

      &:last-of-type {
        border-radius: 0 5px 5px 0;
      }

      &-default {
        border-left: 0;
        border-right: 0;

        &:first-of-type {
          // TODO: Implement color variations if button group should be used again
          border-left: 1px solid var(--oc-color-swatch-primary-default);
        }

        &:last-of-type {
          // TODO: Implement color variations if button group should be used again
          border-right: 1px solid var(--oc-color-swatch-primary-default);
        }
      }
    }
  }
}
</style>
