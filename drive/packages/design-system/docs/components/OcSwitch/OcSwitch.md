---
title: OcSwitch component
next: false
prev: false
---

# OcSwitch component

## Description

The `OcSwitch` component displays a toggle switch. It is a visual representation of a binary choice.

## Accessibility

The `label` property is required to describe the switch to screen readers.

## Examples

### Default

The most basic use case needs a `label` and usually has a `v-model:checked` property to bind the switch state to a variable.

::: livecode {path=/components/OcSwitch/default.vue}
<<< @/components/OcSwitch/default.vue
:::

::: component-api
