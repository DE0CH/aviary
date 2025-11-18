# Claude Code 样式提取与应用

本文档记录了从 Claude Code 扩展 (v2.0.43) 编译产物中提取的样式，并应用到我们的 VSCode IDE Companion 项目中。

## 提取来源

- **路径**: `/Users/jinjing/Downloads/Anthropic.claude-code-2.0.43/extension/webview/index.css`
- **版本**: 2.0.43
- **文件类型**: 编译后的压缩 CSS

## 提取的核心样式类

### 1. Header 样式 (`.he`)

```css
.he {
  display: flex;
  border-bottom: 1px solid var(--app-primary-border-color);
  padding: 6px 10px;
  gap: 4px;
  background-color: var(--app-header-background);
  justify-content: flex-start;
  user-select: none;
}
```

**应用到**: `.chat-header`

**改进点**:

- `gap: 4px` - 更紧凑的间距
- `justify-content: flex-start` - 左对齐而非 space-between
- `background-color: var(--app-header-background)` - 使用独立的 header 背景变量

### 2. Session Selector 按钮 (`.E`)

```css
.E {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 2px 8px;
  background: transparent;
  border: none;
  border-radius: 4px;
  cursor: pointer;
  outline: none;
  min-width: 0;
  max-width: 300px;
  overflow: hidden;
  font-size: var(--vscode-chat-font-size, 13px);
  font-family: var(--vscode-chat-font-family);
}

.E:focus,
.E:hover {
  background: var(--app-ghost-button-hover-background);
}
```

**应用到**: `.session-selector-dropdown select`

**改进点**:

- `background: transparent` - 默认透明背景
- `gap: 6px` - 内部元素间距
- `min-width: 0; max-width: 300px` - 响应式宽度控制
- `overflow: hidden` - 处理文本溢出

### 3. 图标按钮 (`.j`)

```css
.j {
  flex: 0 0 auto;
  padding: 0;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  outline: none;
  width: 24px;
  height: 24px;
}

.j:focus,
.j:hover {
  background: var(--app-ghost-button-hover-background);
}
```

**应用到**: `.new-session-header-button`

**改进点**:

- `flex: 0 0 auto` - 固定尺寸不伸缩
- `border: 1px solid transparent` - 保留边框空间但透明
- 精确的 `24px × 24px` 尺寸

### 4. Session Selector 弹窗 (`.Wt`)

```css
.Wt {
  position: fixed;
  background: var(--app-menu-background);
  border: 1px solid var(--app-menu-border);
  border-radius: var(--corner-radius-small);
  width: min(400px, calc(100vw - 32px));
  max-height: min(500px, 50vh);
  display: flex;
  flex-direction: column;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.1);
  z-index: 1000;
  outline: none;
  font-size: var(--vscode-chat-font-size, 13px);
  font-family: var(--vscode-chat-font-family);
}
```

**应用到**: `.session-selector`

**关键特性**:

- `width: min(400px, calc(100vw - 32px))` - 响应式宽度，小屏幕自适应
- `max-height: min(500px, 50vh)` - 响应式高度
- `box-shadow: 0 4px 16px rgba(0, 0, 0, 0.1)` - 柔和阴影
- 使用 menu 相关的 CSS 变量

### 5. Session List (`.It`, `.St`, `.s`)

```css
/* Content area */
.It {
  padding: 8px;
  overflow-y: auto;
  flex: 1;
  user-select: none;
}

/* List container */
.St {
  display: flex;
  flex-direction: column;
  padding: var(--app-list-padding);
  gap: var(--app-list-gap);
}

/* List item */
.s {
  display: flex;
  align-items: center;
  padding: var(--app-list-item-padding);
  justify-content: space-between;
  background: transparent;
  border: none;
  border-radius: 6px;
  cursor: pointer;
  text-align: left;
  width: 100%;
  font-size: inherit;
  font-family: inherit;
}

.s:hover {
  background: var(--app-list-hover-background);
}

.s.U {
  background: var(--app-list-active-background);
  color: var(--app-list-active-foreground);
}
```

**应用到**: `.session-list`, `.session-item`

**改进点**:

- `border-radius: 6px` - 圆角列表项
- `user-select: none` - 禁止选择文本
- 使用统一的 list 变量系统

## 新增 CSS 变量

从 Claude Code 中提取并添加的 CSS 变量：

```css
/* Header */
--app-header-background: var(--vscode-sideBar-background);

/* List Styles */
--app-list-padding: 0px;
--app-list-item-padding: 4px 8px;
--app-list-border-color: transparent;
--app-list-border-radius: 4px;
--app-list-hover-background: var(--vscode-list-hoverBackground);
--app-list-active-background: var(--vscode-list-activeSelectionBackground);
--app-list-active-foreground: var(--vscode-list-activeSelectionForeground);
--app-list-gap: 2px;

/* Menu Colors */
--app-menu-background: var(--vscode-menu-background);
--app-menu-border: var(--vscode-menu-border);
--app-menu-foreground: var(--vscode-menu-foreground);
--app-menu-selection-background: var(--vscode-menu-selectionBackground);
--app-menu-selection-foreground: var(--vscode-menu-selectionForeground);

/* Ghost Button */
--app-ghost-button-hover-background: var(--vscode-toolbar-hoverBackground);
```

## 设计理念总结

通过分析 Claude Code 的样式，我们发现以下设计理念：

### 1. **响应式优先**

- 使用 `min()` 函数实现响应式尺寸
- 如: `width: min(400px, calc(100vw - 32px))`

### 2. **一致的间距系统**

- 小间距: 4px
- 中间距: 8px
- 大间距: 12px, 16px

### 3. **柔和的视觉效果**

- 透明背景 + hover 时显示背景色
- 柔和的阴影: `0 4px 16px rgba(0, 0, 0, 0.1)`
- 圆角统一使用变量: `var(--corner-radius-small)` = 4px

### 4. **完整的变量系统**

- 所有颜色都通过 CSS 变量定义
- 支持 VSCode 主题自动适配
- 有合理的 fallback 值

### 5. **交互反馈清晰**

- `:hover` 和 `:focus` 状态使用相同样式
- 使用 `var(--app-ghost-button-hover-background)` 统一 hover 背景

## 文件变更

### 修改的文件

1. **`src/webview/App.css`**
   - 更新 Header 样式
   - 更新 Session Selector Modal 样式
   - 添加新的 CSS 变量

### 新增的文件

1. **`src/webview/ClaudeCodeStyles.css`**
   - 完整的 Claude Code 样式提取
   - 包含详细注释和类名映射

2. **`CLAUDE_CODE_STYLES.md`**
   - 本文档，记录样式提取和应用过程

## 效果对比

### 之前

- Header 使用 `justify-content: space-between`
- Session selector 宽度固定 80%
- 阴影较重: `rgba(0, 0, 0, 0.3)`
- 间距不够紧凑

### 之后

- Header 使用 `justify-content: flex-start`，间距 4px
- Session selector 响应式宽度 `min(400px, calc(100vw - 32px))`
- 柔和阴影: `rgba(0, 0, 0, 0.1)`
- 更紧凑的布局，更接近 Claude Code 的视觉风格

## 下一步优化建议

1. **添加选中状态图标** (`.ne` check icon)
2. **实现 session list 的分组显示** (`.te` group header)
3. **添加 session selector button 的图标和箭头** (`.xe`, `.fe`, `.ve` 等)
4. **考虑添加 session 数量徽章**
5. **优化移动端适配**

## 参考资料

- Claude Code Extension: https://marketplace.visualstudio.com/items?itemName=Anthropic.claude-code
- 源文件位置: `/Users/jinjing/Downloads/Anthropic.claude-code-2.0.43/extension/webview/index.css`
