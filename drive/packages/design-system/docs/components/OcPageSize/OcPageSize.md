---
title: OcPageSize component
next: false
prev: false
---

# OcPageSize component

## Description

The `OcPageSize` component can be used to let the user select how many items will be displayed per page.

## Examples

### Default

The component needs to be provided with a `label`, a list of `options` and the currently selected option via `selected`. It emits a `change` event which can be used to change the current selection.

::: livecode {path=/components/OcPageSize/default.vue}
<<< @/components/OcPageSize/default.vue
:::

::: component-api
