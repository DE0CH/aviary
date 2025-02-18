# OcInfoDrop component

## Description

The `OcInfoDrop` component displays given content inside a dropdown menu. It's basically a wrapper of the [OcDrop](./OcDrop) component and is suited to display simple text content.

You might also want to check out the [OcContextualHelper](./OcContextualHelper) component which serves a similar purpose but is more focused on providing help and guidance.

## Accessibility

The component will automatically trap the focus within the dropdown when it's being displayed. The user can close it by pressing the `Escape` key or by clicking on the close button.

## Examples

### Default

The basic usage of the component needs a `title`, and usually involves a `text`. Just like the regular `OcDrop`, it's mostly being used in combination with a button. It's important that the button `id` matches the `toggle` attribute of the dropdown.

::: livecode
```html
<oc-button id="drop-btn">Open drop</oc-button>
<oc-info-drop toggle="#drop-btn" title="Some title" text="Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam." />
```
:::

### List

The following example incorporates a list of items to allow for better structuring of the displayed content.

::: livecode {path=/components/OcInfoDrop/list.vue}
<<< @/components/OcInfoDrop/list.vue
:::
