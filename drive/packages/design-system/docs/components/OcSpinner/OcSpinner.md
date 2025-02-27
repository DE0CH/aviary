---
title: OcSpinner component
next: false
prev: false
---

# OcSpinner component

## Description

The `OcSpinner` component displays a loading spinner to the user, indicating that the system is processing something.

## Accessibility

A loading spinner needs to be described to screen readers via the `aria-label` property.

As soon as the spinner appears, it should programmatically receive focus so that its `aria-label` is read out by the screen reader. For this reason, the component sets `tabindex="-1"` on the element. After the completion of the loading process, the focus should be sent to a reasonable place in the newly loaded content.

## Examples

### Default

The most basic use case should involve an `aria-label`.

::: livecode
```html
<oc-spinner aria-label="Loading content" />
```
:::

### Sizes

These sizes are available: `xsmall`, `small`, `medium`, `large`, `xlarge`, `xxlarge`, `xxxlarge`.

::: livecode
```html
<oc-spinner aria-label="Loading content" size="xsmall" />
<oc-spinner aria-label="Loading content" size="small" />
<oc-spinner aria-label="Loading content" size="medium" />
<oc-spinner aria-label="Loading content" size="large" />
<oc-spinner aria-label="Loading content" size="xlarge" />
<oc-spinner aria-label="Loading content" size="xxlarge" />
<oc-spinner aria-label="Loading content" size="xxxlarge" />
```
:::

::: component-api
