# OpenCloud ts-config

[![Matrix](https://img.shields.io/matrix/opencloud%3Amatrix.org?logo=matrix)](https://app.element.io/#/room/#opencloud:matrix.org)

This package provides a reusable TypeScript config for application and extension development within the OpenCloud Web ecosystem.

## Installation

Depending on your package manager, run one of the following commands:

```
$ npm install @opencloud-eu/ts-config --save-dev

$ pnpm add -D @opencloud-eu/ts-config

$ yarn add @opencloud-eu/ts-config --dev
```

## Usage

To extend your TypeScript config with the OpenCloud TypeScript config, add the following to your `tsconfig.json` file:

```json
{
  "extends": "@opencloud-eu/tsconfig"
}
```
