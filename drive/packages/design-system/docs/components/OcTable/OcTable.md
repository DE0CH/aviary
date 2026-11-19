---
title: OcTable component
next: false
prev: false
---

# OcTable component

## Description

The `OcTable` component represents a table with dynamically generated columns and rows.

There is also a simpler form of the table in the [OcTableSimple](./OcTableSimple) component. It can't be generated dynamically and needs to be built manually.

## Examples

### Default

The most basic use case needs `fields` and `data` properties.

::: livecode {path=/components/OcTable/default.vue}
<<< @/components/OcTable/default.vue
:::

### Fields with slots

`fields` can also be defined as slots. Additionally, the table supports a slot or the `footer`.

::: livecode {path=/components/OcTable/slots.vue}
<<< @/components/OcTable/slots.vue{3-9,20,26}
:::

### Disabling and highlighting rows

Specific rows can be `disabled` or `highlighted` by referencing the rows by their respective item ids.

::: livecode {path=/components/OcTable/disablingHighlighting.vue}
<<< @/components/OcTable/disablingHighlighting.vue{5,6}
:::

::: component-api
