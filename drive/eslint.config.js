import openCloudConfig from '@opencloud-eu/eslint-config'

export default [...openCloudConfig, {
  "overrides": [
    {
      "files": ["packages/design-system/docs/**/*.vue"],
      "rules": {
        "vue/multi-word-component-names": "off"
      }
    }
  ]
}]
