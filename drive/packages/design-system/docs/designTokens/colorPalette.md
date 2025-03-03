# Color Palette

The design system uses a predefined color palette to ensure a consistent look and feel across the interface. The color palette is designed to be accessible and to provide a wide range of colors for different use cases.

## Available colors

<script setup lang="ts">
import { computed } from 'vue'
import designTokens from '../../src/assets/tokens/ods.json'

const tokens = computed(() => {
	return Object.values(designTokens).filter((token) => token.name.startsWith('oc-color-'))
})

const fields = [
   {
    name: 'color',
    title: 'Color',
    type: 'slot'
  },
  {
    name: 'name',
    title: 'Name',
    type: 'slot'
  },
  {
    name: 'value',
    title: 'Value',
    type: 'slot'
  },
]
</script>

<oc-table :fields="fields" :data="tokens">
  <template #color="{ item }">
    <div :style="{ backgroundColor: item.value, width: '150px', height: '50px' }" />
  </template>
  <template #name="{ item }">
    {{ item.name }}
  </template>
  <template #value="{ item }">
    {{ item.value }}
  </template>
</oc-table>

<style lang="scss">
.oc-tbody-tr {
  background-color: var(--oc-color-background) !important;
}
</style>

## Usage

You can use these variables in your SCSS files or style blocks:

```scss
.element {
  color: var(--oc-color-swatch-primary-default);
  background-color: var(--oc-color-background-highlight);
}
```
