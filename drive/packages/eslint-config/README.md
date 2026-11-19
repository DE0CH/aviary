# OpenCloud eslint-config

[![Matrix](https://img.shields.io/matrix/opencloud%3Amatrix.org?logo=matrix)](https://app.element.io/#/room/#opencloud:matrix.org)

This package provides a reusable eslint config for application and extension development within the OpenCloud Web ecosystem.

## Installation

Depending on your package manager, run one of the following commands:

```
$ npm install @opencloud-eu/eslint-config --save-dev

$ pnpm add -D @opencloud-eu/eslint-config

$ yarn add @opencloud-eu/eslint-config --dev
```

## Usage

To extend your eslint config with the OpenCloud eslint config, add the following to your `.eslint.config.js` file:

```js
import openCloudConfig from '@opencloud-eu/eslint-config'

export default [...openCloudConfig]
```
