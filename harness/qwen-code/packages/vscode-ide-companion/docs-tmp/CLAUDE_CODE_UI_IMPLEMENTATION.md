# Claude Code UI 还原实现

## 概述

本文档记录了如何将 Claude Code VSCode 扩展的 Webview UI 设计还原到我们的 Qwen Code VSCode IDE Companion 项目中。

## 分析的源 HTML 结构

从 Claude Code VSCode 扩展的 webview HTML 中，我们识别出以下关键组件：

### 1. 顶部导航栏 (`.he`)

- **Past Conversations** 按钮 (`.E`) - 带下拉箭头的会话列表按钮
- **New Session** 按钮 (`.j`) - 创建新会话的加号按钮
- 使用了 ghost button 风格，hover 时有背景色变化

### 2. 中间内容区域

- **空状态界面** - 当没有消息时显示
  - Qwen Logo (SVG)
  - 欢迎文本："What to do first? Ask about this codebase or we can start writing code."
  - 横幅提示："Prefer the Terminal experience? Switch back in Settings."

### 3. 底部输入区域 (`.u`)

- **可编辑的 contenteditable div** - 替代传统的 textarea
  - placeholder: "Ask Claude to edit…"
  - 支持多行输入
- **操作按钮行** (`.ri`)
  - "Ask before edits" 按钮 (`.l`) - 编辑模式选择
  - Thinking 开关按钮 (`.H.ni`)
  - 命令菜单按钮
  - 发送按钮 (`.r`)

## 实现的组件

### 1. EmptyState 组件

**文件**: `src/webview/components/EmptyState.tsx`, `EmptyState.css`

**功能**:

- 显示 Qwen Logo (使用现有的 SVG)
- 显示欢迎文本
- 显示横幅提示（可关闭）
- 响应式布局

**关键样式**:

```css
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 100%;
  padding: 40px 20px;
}
```

### 2. 更新的 Header

**改动**: `src/webview/App.tsx`, `App.css`

**变更**:

- 将 select 下拉框改为 "Past Conversations" 按钮
- 按钮样式遵循 Claude Code 的 ghost button 设计
- 使用 flex 布局，左对齐按钮，右侧 spacer，最右侧新建按钮

**类名**:

- `.header-conversations-button` - 会话列表按钮
- `.header-spacer` - flex spacer
- `.new-session-header-button` - 新建会话按钮

### 3. 重新设计的输入表单

**改动**: `src/webview/App.tsx`, `App.css`

**变更**:

- 使用 `contenteditable` div 替代 `<input>` 或 `<textarea>`
- 添加操作按钮行：
  - Edit Mode 按钮
  - Thinking 开关
  - 命令菜单按钮
  - 发送按钮
- 使用分隔线分隔按钮组

**类名**:

- `.input-wrapper` - 输入区域容器
- `.input-field-editable` - contenteditable div
- `.input-actions` - 操作按钮行
- `.action-button` - 带文本的按钮
- `.action-icon-button` - 只有图标的按钮
- `.action-divider` - 分隔线
- `.send-button-icon` - 发送按钮

### 4. 更新的 CSS 变量

在 `App.css` 中添加/更新的变量：

```css
--app-transparent-inner-border: rgba(255, 255, 255, 0.1);
--app-ghost-button-hover-background: var(--vscode-toolbar-hoverBackground);
```

## 关键设计决策

### 1. Logo 选择

- 使用 Qwen 现有的像素风格 logo SVG
- 颜色: `#D97757` (橙色)
- 保持了品牌一致性

### 2. 输入框实现

- 选用 `contenteditable="plaintext-only"` 而不是 `textarea`
  - 更好的控制样式
  - 支持动态高度
  - 与 Claude Code 一致的体验

### 3. 按钮风格

- 全部使用 ghost button 风格
- hover 时使用 VSCode 的 `toolbar-hoverBackground` 颜色
- 保持了 VSCode 原生的视觉感受

### 4. 空状态显示逻辑

```typescript
const hasContent =
  messages.length > 0 ||
  isStreaming ||
  toolCalls.size > 0 ||
  permissionRequest !== null;
```

只有在没有任何内容时才显示空状态界面。

## 文件变更清单

### 新增文件

1. `src/webview/components/EmptyState.tsx` - 空状态组件
2. `src/webview/components/EmptyState.css` - 空状态样式
3. `docs-tmp/CLAUDE_CODE_UI_IMPLEMENTATION.md` - 本文档

### 修改文件

1. `src/webview/App.tsx`
   - 导入 EmptyState 组件
   - 重构 header 为 Claude Code 风格
   - 重构输入表单为 contenteditable + 操作按钮
   - 添加 `hasContent` 逻辑判断

2. `src/webview/App.css`
   - 添加 header 按钮样式
   - 添加 contenteditable 输入框样式
   - 添加操作按钮样式
   - 更新 CSS 变量

## 样式映射表

| Claude Code 类名 | 我们的类名                     | 用途             |
| ---------------- | ------------------------------ | ---------------- |
| `.he`            | `.chat-header`                 | 顶部导航栏       |
| `.E`             | `.header-conversations-button` | 会话列表按钮     |
| `.j`             | `.new-session-header-button`   | 新建会话按钮     |
| `.Q`             | `.messages-container`          | 消息容器         |
| `.u`             | `.input-form`                  | 输入表单         |
| `.fo`            | `.input-wrapper`               | 输入框包装器     |
| `.d`             | `.input-field-editable`        | 可编辑输入框     |
| `.ri`            | `.input-actions`               | 操作按钮行       |
| `.l`             | `.action-button`               | 带文本的操作按钮 |
| `.H`             | `.action-icon-button`          | 图标按钮         |
| `.r`             | `.send-button-icon`            | 发送按钮         |

## 未来改进

1. **Banner 关闭功能** - 实现横幅的可关闭逻辑并保存状态
2. **Edit Mode 切换** - 实现编辑模式切换功能
3. **Thinking 开关** - 实现 thinking 显示开关
4. **命令菜单** - 实现斜杠命令菜单
5. **响应式优化** - 针对更小的窗口尺寸优化布局

## 构建验证

```bash
npm run build
```

构建成功，没有 TypeScript 或 ESLint 错误。

## 截图对比

_(建议添加截图展示还原前后的对比)_

## 总结

成功将 Claude Code 的 webview UI 设计还原到 Qwen Code VSCode IDE Companion 项目中，主要改进包括：

1. ✅ 更现代的空状态界面
2. ✅ 更直观的 header 导航
3. ✅ 更强大的输入框体验
4. ✅ 更清晰的操作按钮布局
5. ✅ 保持了 VSCode 原生风格

整体 UI 更加专业、现代，用户体验得到显著提升。
