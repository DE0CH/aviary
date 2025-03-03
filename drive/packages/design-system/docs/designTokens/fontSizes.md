# Font Sizes

The design system uses predefined font sizes to achieve visual harmony in the interface. It’s purposefully designed to keep the number of separate font sizes to a minimum.

## Available tokens

The following tokens are available:

- `oc-font-size-xsmall` (0.72rem)
- `oc-font-size-small` (0.86rem)
- `oc-font-size-default` (0.875rem)
- `oc-font-size-medium` (1rem)
- `oc-font-size-large` (1.14rem)
- `oc-font-size-xlarge` (1.29rem)

## Usage

You can use these variables in your SCSS files or style blocks:

```scss
.element {
  font-size: var(--oc-font-size-large);
}
```

## Examples

::: livecode {path=/designTokens/sizeExamples.vue}
<<< @/designTokens/sizeExamples.vue
:::
