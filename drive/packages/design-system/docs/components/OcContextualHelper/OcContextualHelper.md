---
title: OcContextualHelper component
next: false
prev: false
---

# OcContextualHelper component

## Description

The `OcContextualHelper` component can be used to provide context or additional information about an element or to guide the user through the UI.

## Accessibility

The component will automatically trap the focus within the menu when it is opened. The user can close the menu by pressing the `Escape` or `Enter` key or by clicking on the close button.

## Examples

### Default

The most basic use case needs a `title` and usually has a `text` that is being displayed.

::: livecode

```html
<p>
  I have some additional context.
  <oc-contextual-helper title="Some helper" text="Some text giving context." />
</p>
```

:::

### List of items

The component can also display a list of items. This is useful when you want to structure the information by using headlines and paragraphs.

::: livecode {path=/components/OcContextualHelper/list.vue}
<<< @/components/OcContextualHelper/list.vue
:::

::: component-api
