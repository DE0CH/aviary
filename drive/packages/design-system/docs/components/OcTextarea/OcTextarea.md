---
title: OcTextarea component
next: false
prev: false
---

# OcTextarea component

## Description

`OcTextarea`s allow users to provide text input. Commonly used when the expected input is long.
For short input, use the `OcTextInput` component.

## Accessibility

The label is required and represents the name of the textarea.

The description-message can be used additionally to give further information about the textarea. When a
description is given, it will be referenced via the `aria-describedby` property automatically.
An error or warning will replace the description as well as the `aria-describedby` property until the error
or warning is fixed.

## Examples

### Default

The default and most simple use case involves a `v-model` and a `label`.

::: livecode {path=/components/OcTextarea/default.vue}
<<< @/components/OcTextarea/default.vue
:::

### Disabled

::: livecode

```vue
<oc-textarea disabled label="Disabled" model-value="I am disabled" />
```

:::

### Messages

::: livecode {path=/components/OcTextarea/messages.vue}
<<< @/components/OcTextarea/messages.vue
:::

::: component-api
