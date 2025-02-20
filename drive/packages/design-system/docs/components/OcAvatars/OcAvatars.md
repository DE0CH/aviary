---
title: OcAvatars component
next: false
prev: false
---

# OcAvatars component

## Description

The `OcAvatars` component represents a collection of avatars.

## Accessibility

The component can be provided with an `accessible-description` since the avatars are hidden for screen readers.

## Examples

### Default

::: livecode
```vue
<oc-avatars
	:items="[
		{ displayName: 'Alan', shareType: 0 },
		{ displayName: 'Mary', shareType: 0 },
		{ displayName: 'Brian', shareType: 0 },
		{ displayName: 'Engineers', shareType: 1 }
	]"
/>
```
:::

### Stacked avatars

Avatars can be stacked when there is limited screen space.

::: livecode
```vue
<oc-avatars
	:items="[
		{ displayName: 'Alan', shareType: 0 },
		{ displayName: 'Mary', shareType: 0 },
		{ displayName: 'Brian', shareType: 0 }
	]"
	accessible-description="This resource is shared with many users."
	:stacked="true"
/>
```
:::

### Limited max amount

The maximum amount of avatars can be limited.

::: livecode
```vue
<oc-avatars
	:items="[
		{ displayName: 'Alan', shareType: 0 },
		{ displayName: 'Mary', shareType: 0 },
		{ displayName: 'Brian', shareType: 0 }
	]"
	accessible-description="This resource is shared with many users."
	max-displayed="2"
/>
```
:::

::: component-api
