# OcPagination component

## Description

The `OcPagination` component displays a list of links used for switching to different pages.

## Examples

### Default

The component needs to be provided with the `currentPage`, the `currentRoute` and the total number of `pages`.

::: livecode
```html
<oc-pagination :pages="3" :currentPage="1" :currentRoute="{ name: 'folderA' }" />
<oc-pagination :pages="3" :currentPage="2" :currentRoute="{ name: 'folderB' }" />
<oc-pagination :pages="3" :currentPage="3" :currentRoute="{ name: 'folderC' }" />
```
:::

### Limit max displayed

The amount of displayed pages can be limited via the `max-displayed` property.

::: livecode
```html{5}
<oc-pagination
	:pages="10"
	:currentPage="3"
	:currentRoute="{ name: 'folderC' }"
	:max-displayed="3"
/>
```
:::
