---
title: OcDefinitionList component
next: false
prev: false
---

# OcDefinitionList component

## Description

The `OcDefinitionList` component displays a description list. This is especially useful when displaying a list of key-value pairs.

## Examples

### Default

The default use case needs a list of items to display. Each item should have a `term` and a `definition`.

::: livecode
```html
<oc-definition-list
	:items="[
		{ term: 'Files', definition: '3' },
		{ term: 'Folders', definition: '4' },
		{ term: 'Spaces', definition: '2' }
	]"
/>
```
:::

::: component-api
