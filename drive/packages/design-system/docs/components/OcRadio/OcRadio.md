---
title: OcRadio component
next: false
prev: false
---

# OcRadio component

## Description

The `OcRadio` component represents a simple radio input element that can either be checked or unchecked.

## Accessibility

The `label` is required and represents the name of the input. If `label-hidden` is set to `true`, the label will be hidden from the screen but still be available for screen readers via the `aria-label` attribute.

## Examples

### Default

The default use case needs a `label` and a `v-model` to bind the value of the radio element.

::: livecode {path=/components/OcRadio/default.vue}
<<< @/components/OcRadio/default.vue
:::

### Group

Multiple radio elements can be grouped to allow choosing one option.

::: livecode {path=/components/OcRadio/group.vue}
<<< @/components/OcRadio/group.vue
:::

::: component-api
