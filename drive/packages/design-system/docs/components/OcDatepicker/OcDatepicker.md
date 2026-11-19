---
title: OcDatepicker component
next: false
prev: false
---

# OcDatepicker component

## Description

The `OcDatepicker` component can be used to enter or select a date from a calendar. The component uses the browser's native date picker when available and does not rely on any third-party libraries. As a result, the visual appearance of the date picker may vary depending on the browser.

## Accessibility

The `label` is required and represents the name of the input.

## Examples

### Default

The default use case needs a `label` and usually has a `v-model` to bind the value of the date picker.

::: livecode {path=/components/OcDatepicker/default.vue}
<<< @/components/OcDatepicker/default.vue
:::

### Event handler

The component emits a `date-changed` event. Using this can be useful when you want to do some custom handling on date change (e.g. validation) or when you want to check if the input caused some errors.

In the example below, `min-date` is set to the current day. That means entering a date before that will result in an error.

::: livecode {path=/components/OcDatepicker/handler.vue}
<<< @/components/OcDatepicker/handler.vue
:::

::: component-api
