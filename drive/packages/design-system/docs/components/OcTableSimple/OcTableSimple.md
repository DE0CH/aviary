---
title: OcTableSimple component
next: false
prev: false
---

# OcTableSimple component

## Description

The `OcTableSimple` component represents a simple table. As opposed to the [OcTable](./OcTable) component, it can't be generated dynamically and needs to be built manually.

## Examples

### Default

The component provides a default slot that can be filled with standard HTML table elements (or their equivalents from this design system).

::: livecode
```html
<oc-table-simple>
	<oc-table-head>
		<oc-table-tr>
			<oc-table-th>ID</oc-table-th>
			<oc-table-th>Filename</oc-table-th>
			<oc-table-th>Size</oc-table-th>
		</oc-table-tr>
	</oc-table-head>
	<oc-table-body>
		<oc-table-tr>
			<oc-table-td>83558362-3fc6-4b96-a2e5-dba7435c4fae</oc-table-td>
			<oc-table-td>textfile.txt</oc-table-td>
			<oc-table-td>50</oc-table-td>
		</oc-table-tr>
		<oc-table-tr>
			<oc-table-td>fbd793d3-c36c-4f92-bff6-dfeebaec8248</oc-table-td>
			<oc-table-td>Folder</oc-table-td>
			<oc-table-td>9482</oc-table-td>
		</oc-table-tr>
	</oc-table-body>
</oc-table-simple>
```
:::

::: component-api
