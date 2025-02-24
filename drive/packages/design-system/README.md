# OpenCloud Design System

[![Matrix](https://img.shields.io/matrix/opencloud%3Amatrix.org?logo=matrix)](https://app.element.io/#/room/#opencloud:matrix.org)

The **OpenCloud Design System** provides components and utilities for application and extension development within the
OpenCloud Web ecosystem. It can be developed standalone via the design system documentation. The documentation is
built with [VitePress](https://vitepress.dev/).

Head over to the [hosted docs](https://design.opencloud.eu/) for more information!

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

To use the components and utilities of the design-system in your application, you first need to install the package. Depending on your package manager, run one of the following commands:

```
$ npm install @opencloud-eu/design-system --save-dev

$ pnpm add -D @opencloud-eu/design-system

$ yarn add @opencloud-eu/design-system --dev
```

It's recommended to install this package as a dev dependency because it's only really needed for providing autocompletion in your IDE and unit tests. In a runtime context, the OpenCloud Web runtime provides the actual implementation.
