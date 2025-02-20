---
title: OcAvatarItem component
next: false
prev: false
---

# OcAvatarItem component

## Description

The `OcAvatarItem` component displays a given icon as an avatar.

## Accessibility

The component can be provided with an `accessible-label` in case the avatar item is used alone. In case the avatar item is used next to a username or display name, it should be left empty. If not specified, an avatar item will get `aria-hidden="true"`.

## Examples

### Default

::: livecode
```vue
<oc-avatar-item name="Cloud" icon="cloud" />
<oc-avatar-item name="Home" icon="home" />
<oc-avatar-item name="Settings" icon="settings" />
```
:::

::: component-api
