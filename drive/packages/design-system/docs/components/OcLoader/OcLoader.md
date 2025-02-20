---
title: OcLoader component
next: false
prev: false
---

# OcLoader component

## Description

The `OcLoader` component can be used to give feedback about an ongoing process.

## Accessibility

An `aria-label` attribute should be provided to describe the loader's purpose for screen readers.

## Examples

### Default

The most basic use case involves an `aria-label` property to describe the loader's purpose.

::: livecode
```html
<oc-loader aria-label="Loading content" />
```
:::

### Flat

The loader also comes in a flat version.

::: livecode
```html
<oc-loader aria-label="Loading content" :flat="true" />
```
:::

::: component-api
