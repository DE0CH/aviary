# OcList component

## Description

The `OcList` component can be used to display lists.

## Examples

### Default

The components provides a default slot to display the list items.

::: livecode
```html
<div>
	<oc-list>
		<li>First element</li>
		<li>Second element</li>
		<li>Third element</li>
	</oc-list>
</div>
<div class="oc-mt-m">
	<oc-list :raw="true">
		<li>First element</li>
		<li>Second element</li>
		<li>Third element</li>
	</oc-list>
</div>
```
:::
