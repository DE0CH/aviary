module.exports = {
  printWidth: 100,
  singleQuote: true,
  semi: false,
  trailingComma: 'none',
  overrides: [
    {
      files: '*.vue',
      options: {
        parser: 'vue'
      }
    }
  ]
}
