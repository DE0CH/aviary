/** @type {import('tailwindcss').Config} */
// eslint-disable-next-line no-undef
module.exports = {
  content: [
    // 渐进式采用策略：只扫描新创建的Tailwind组件
    './src/webview/App.tsx',
    './src/webview/components/ui/**/*.{js,jsx,ts,tsx}',
    './src/webview/components/messages/**/*.{js,jsx,ts,tsx}',
    './src/webview/components/toolcalls/**/*.{js,jsx,ts,tsx}',
    './src/webview/components/InProgressToolCall.tsx',
    './src/webview/components/MessageContent.tsx',
    './src/webview/components/InfoBanner.tsx',
    './src/webview/components/InputForm.tsx',
    './src/webview/components/PermissionDrawer.tsx',
    './src/webview/components/PlanDisplay.tsx',
    // 当需要在更多组件中使用Tailwind时，可以逐步添加路径
    // "./src/webview/components/NewComponent/**/*.{js,jsx,ts,tsx}",
    // "./src/webview/pages/**/*.{js,jsx,ts,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        qwen: {
          orange: '#615fff',
          'clay-orange': '#4f46e5',
          ivory: '#f5f5ff',
          slate: '#141420',
          green: '#6bcf7f',
        },
      },
      borderRadius: {
        small: '4px',
        medium: '6px',
        large: '8px',
      },
      spacing: {
        small: '4px',
        medium: '8px',
        large: '12px',
        xlarge: '16px',
      },
    },
  },
  plugins: [],
};
