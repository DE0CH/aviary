# OcLogo component

## Description

The `OcLogo` component can be used to display logos. It's basically a wrapper of the [OcImage](./OcImage.md) component, with a fixed `max-width` and `max-height`, and a bit of `margin`.

## Accessibility

An `alt` attribute needs to be provided to describe the image content for screen readers.

## Examples

### Default

The basic usage of the component needs the image `src` and an `alt` property.

::: livecode
```html
<oc-logo  src="https://opencloud.eu/themes/cloudy/src/assets/favicon/favicon.svg"  alt="OpenCloud logo" />
```
:::
