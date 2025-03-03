---
title: OcProgress component
next: false
prev: false
---

# OcProgress component

## Description

The `OcProgress` component displays a progress bar.

## Examples

### Default

The default usage of the component involves the `value` and `max` properties.

::: livecode

```html
<oc-progress :value="4" :max="10" />
```

:::

### Indeterminate

The component can be set to an indeterminate state by setting `indeterminate` to `true`.

::: livecode

```html
<oc-progress :indeterminate="true" />
```

:::

### Variations

Different variations can be applied to the progress bar by setting the `variation` property to one of the following values: `primary`, `passive`, `danger`, `success`, `warning`.

::: livecode

```html
<oc-progress variation="primary" :value="4" :max="10" class="oc-mb-m" />
<oc-progress variation="passive" :value="4" :max="10" class="oc-mb-m" />
<oc-progress variation="danger" :value="4" :max="10" class="oc-mb-m" />
<oc-progress variation="success" :value="4" :max="10" class="oc-mb-m" />
<oc-progress variation="warning" :value="4" :max="10" />
```

:::

::: component-api
