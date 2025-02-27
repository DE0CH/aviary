---
title: OcModal component
next: false
prev: false
---

# OcModal component

## Description

The `OcModal` component can be used to display a modal to the user. Modals are generally used to force the user to focus on confirming or completing a single action.

Every modal gets automatically added a background which spans the whole width and height. The modal itself is aligned to center both vertically and horizontally.

## Examples

### Default

The most common use case of the component is in combination with a button that opens the modal. A modal needs at least a `title` attribute.

::: livecode {path=/components/OcModal/default.vue}
<<< @/components/OcModal/default.vue
:::

### Input

A modal can have an input field.

::: livecode {path=/components/OcModal/input.vue}
<<< @/components/OcModal/input.vue{6,7,26}
:::

### Slot

The component provides a `content` slot to add custom content. This can even be a dynamically loaded component.

It can also be used for displaying custom modal actions. In this case, the actions need to be implemented manually, and `hide-actions` needs to be set to `true` so the default actions stay hidden.

::: livecode {path=/components/OcModal/slot.vue}
<<< @/components/OcModal/slot.vue{4-32}
:::

::: component-api
