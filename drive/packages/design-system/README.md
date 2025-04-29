# OpenCloud Design System

[![Matrix](https://img.shields.io/matrix/opencloud%3Amatrix.org?logo=matrix)](https://app.element.io/#/room/#opencloud:matrix.org)

The **OpenCloud Design System** provides components and utilities for application and extension development within the
OpenCloud Web ecosystem. It can be developed standalone via the design system documentation. The documentation is
built with [VitePress](https://vitepress.dev/).

Head over to the [hosted docs](https://docs.opencloud.eu/design-system/) for more information!

## Running the docs locally

To run the docs, you first need to install all dependencies:

```
pnpm i
```

Start docs in dev mode:

```
pnpm docs:dev
```

## Usage as a package

### Installation

To use the design-system in your application, you first need to install the package. Depending on your package manager, run one of the following commands:

```
$ npm install @opencloud-eu/design-system

$ pnpm add -D @opencloud-eu/design-system

$ yarn add @opencloud-eu/design-system
```

### Styles

In order to use the provided CSS classes and to ensure the components are styled correctly, you need to import the styles like so:

```
import '@opencloud-eu/design-system/dist/design-system.css'
```

### Components

To use the components, you need to import the component you want to use. For example, to use the `OcButton` component:

```
import { OcButton } from '@opencloud-eu/design-system/components'

<oc-button>
  Click me!
</oc-button>
```
