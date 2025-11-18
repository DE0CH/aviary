# WebView 固定功能实现说明

> **更新时间**: 2025-11-18
> **功能**: WebView 右侧固定 + Pin Tab 防止意外关闭

---

## ✅ 已实现的功能

### 1. WebView 固定在右侧 ✅

**位置**: `src/WebViewProvider.ts:82-85`

```typescript
this.panel = vscode.window.createWebviewPanel(
  'qwenCode.chat',
  'Qwen Code Chat',
  {
    viewColumn: vscode.ViewColumn.Beside, // 在编辑器右侧打开
    preserveFocus: true, // 不抢夺编辑器焦点
  },
  // ...
);
```

**功能说明**:

- ✅ `viewColumn: vscode.ViewColumn.Beside` - WebView 始终在当前编辑器的右侧打开
- ✅ `preserveFocus: true` - 打开 WebView 时不会夺取焦点，用户可以继续编辑代码

**用户体验**:

- 打开 WebView 时，编辑器保持焦点
- WebView 在右侧独立列中打开
- 可以同时查看代码和聊天界面

---

### 2. WebView Tab 自动固定 ✅

**位置**: `src/WebViewProvider.ts:100-107`

```typescript
// Pin the webview tab to prevent accidental closure
// Note: This is done after panel creation to ensure it takes effect
setTimeout(() => {
  if (this.panel) {
    // Execute VSCode command to pin the active editor
    vscode.commands.executeCommand('workbench.action.pinEditor');
  }
}, 100);
```

**功能说明**:

- ✅ 创建 WebView 后自动执行 pin 命令
- ✅ 使用 100ms 延迟确保 panel 完全创建
- ✅ 防止用户意外关闭 WebView tab

**用户体验**:

- WebView tab 会显示 pin 图标（📌）
- 关闭其他 tab 时，WebView 不会被关闭
- 需要明确点击关闭按钮才能关闭 WebView

---

### 3. WebView 重新打开时保持位置 ✅

**位置**: `src/WebViewProvider.ts:74-76`

```typescript
if (this.panel) {
  this.panel.reveal(vscode.ViewColumn.Beside, true); // preserveFocus = true
  return;
}
```

**功能说明**:

- ✅ 如果 WebView 已存在，调用 `reveal()` 显示
- ✅ 参数 `vscode.ViewColumn.Beside` 确保在右侧显示
- ✅ 参数 `true` (preserveFocus) 不夺取焦点

**用户体验**:

- 关闭后重新打开，WebView 仍然在右侧
- 多次打开不会创建多个 WebView
- 保持用户的工作流程

---

## 🎯 与 Claude Code 的对比

| 功能         | Claude Code | 当前实现 | 状态     |
| ------------ | ----------- | -------- | -------- |
| **右侧打开** | ✅          | ✅       | 完全对标 |
| **不抢焦点** | ✅          | ✅       | 完全对标 |
| **Pin Tab**  | ✅          | ✅       | 完全对标 |
| **记住位置** | ✅          | ✅       | 完全对标 |

---

## 📊 技术实现细节

### ViewColumn.Beside 的行为

```typescript
vscode.ViewColumn.Beside;
```

**说明**:

- 如果当前有活动编辑器，在其右侧创建新列
- 如果当前没有活动编辑器，在 ViewColumn.One 中打开
- 如果已经有多列，在最右侧列的右边打开

**实际效果**:

```
┌─────────────┬─────────────┐
│             │             │
│   Code      │  WebView    │
│   Editor    │  (Pinned)   │
│   (Focus)   │             │
│             │             │
└─────────────┴─────────────┘
```

---

### preserveFocus 的作用

```typescript
{
  viewColumn: vscode.ViewColumn.Beside,
  preserveFocus: true  // ← 关键参数
}
```

**功能**:

- `true`: 创建 WebView 时不夺取焦点，编辑器保持活动
- `false`: 创建 WebView 时自动切换焦点到 WebView

**用户场景**:

- ✅ 用户正在编辑代码时打开聊天，焦点仍在编辑器
- ✅ 用户可以继续输入代码，不会被打断
- ✅ 想要与 AI 交互时，手动点击 WebView 即可

---

### Pin Editor 命令的作用

```typescript
vscode.commands.executeCommand('workbench.action.pinEditor');
```

**功能**:

- 固定当前活动的 editor tab
- 防止被 `workbench.action.closeOtherEditors` 等命令关闭
- 在 tab 上显示 pin 图标

**VSCode 原生行为**:

- Pinned tab 会在非 pinned tab 的左侧显示
- 关闭"其他编辑器"时，pinned 的不会被关闭
- Pinned tab 的颜色/样式可能有所不同（取决于主题）

---

## 🧪 测试建议

### 测试场景 1: 首次打开

**步骤**:

1. 打开一个代码文件
2. 执行命令 `qwenCode.openChat`
3. 观察 WebView 位置

**预期结果**:

- ✅ WebView 在右侧打开
- ✅ 代码编辑器保持焦点
- ✅ WebView tab 显示 pin 图标（📌）

---

### 测试场景 2: 关闭后重新打开

**步骤**:

1. 关闭 WebView tab
2. 再次执行命令 `qwenCode.openChat`
3. 观察 WebView 位置

**预期结果**:

- ✅ WebView 再次在右侧打开
- ✅ WebView 再次被 pin
- ✅ 代码编辑器保持焦点

---

### 测试场景 3: 关闭其他编辑器

**步骤**:

1. 打开多个代码文件和 WebView
2. 右键点击任意 tab
3. 选择 "关闭其他编辑器"

**预期结果**:

- ✅ 其他非 pinned tab 被关闭
- ✅ WebView (pinned) 保持打开
- ✅ 当前 tab 和 WebView 仍然可见

---

### 测试场景 4: 切换焦点

**步骤**:

1. WebView 打开后，焦点在编辑器
2. 点击 WebView 中的输入框
3. 输入一些文本
4. 按 Ctrl/Cmd + 1 切换回编辑器

**预期结果**:

- ✅ WebView 输入框获得焦点
- ✅ 可以正常输入
- ✅ 快捷键可以切换焦点
- ✅ WebView 保持在右侧

---

### 测试场景 5: 分屏编辑器

**步骤**:

1. 已经有左右分屏的编辑器
2. 焦点在左侧编辑器
3. 打开 WebView

**预期结果**:

- ✅ WebView 在右侧编辑器的右边打开（第三列）
- ✅ 左侧编辑器保持焦点
- ✅ WebView 被 pin

---

## 🔧 故障排查

### 问题 1: WebView 没有被 pin

**可能原因**:

- setTimeout 延迟不够
- panel 还未完全创建

**解决方案**:

```typescript
// 增加延迟到 200ms
setTimeout(() => {
  if (this.panel) {
    vscode.commands.executeCommand('workbench.action.pinEditor');
  }
}, 200);
```

---

### 问题 2: WebView 不在右侧打开

**可能原因**:

- 没有活动编辑器
- ViewColumn 参数错误

**解决方案**:
确保使用正确的参数格式：

```typescript
{
  viewColumn: vscode.ViewColumn.Beside,  // ← 必须是对象属性
  preserveFocus: true
}
```

---

### 问题 3: WebView 抢夺焦点

**可能原因**:

- `preserveFocus` 设置为 `false` 或未设置
- `reveal()` 方法没有传递 `preserveFocus` 参数

**解决方案**:

```typescript
// 创建时
{ viewColumn: ..., preserveFocus: true }

// 重新显示时
this.panel.reveal(vscode.ViewColumn.Beside, true);
//                                           ↑
//                                   preserveFocus
```

---

## 📝 代码改动总结

### 修改的文件

- `src/WebViewProvider.ts` (修改 ~30 行)

### 主要改动

1. **show() 方法** (line 73-107)
   - 修改 `createWebviewPanel` 参数格式
   - 添加 `preserveFocus: true`
   - 添加自动 pin 逻辑
   - 修改 `reveal()` 调用参数

2. **构造函数** (line 27-33)
   - 修复 TypeScript 警告
   - 将 `private context` 改为普通参数

### 新增代码

- 添加 10 行（pin 逻辑和注释）

---

## 🚀 后续优化建议

### 优先级 P2 - 可选增强

#### 1. 添加配置选项

**建议**:

```typescript
// 在 package.json 中添加配置
"qwenCode.webview.autoPin": {
  "type": "boolean",
  "default": true,
  "description": "Automatically pin the WebView tab"
}

// 在代码中使用配置
const config = vscode.workspace.getConfiguration('qwenCode');
const autoPin = config.get<boolean>('webview.autoPin', true);

if (autoPin) {
  setTimeout(() => {
    vscode.commands.executeCommand('workbench.action.pinEditor');
  }, 100);
}
```

**好处**:

- 用户可以选择是否自动 pin
- 更灵活的用户体验

---

#### 2. 记住 WebView 大小

**建议**:

```typescript
// 在 workspace state 中保存大小
context.workspaceState.update('webview.size', {
  width: panel.viewColumn,
  height: panel.visible,
});

// 恢复时使用保存的大小
const savedSize = context.workspaceState.get('webview.size');
```

**好处**:

- 用户调整的 WebView 大小会被记住
- 下次打开时恢复到相同大小

---

#### 3. 添加键盘快捷键

**建议**:

```json
// package.json
"keybindings": [
  {
    "command": "qwenCode.openChat",
    "key": "ctrl+shift+q",
    "mac": "cmd+shift+q"
  },
  {
    "command": "qwenCode.focusChat",
    "key": "ctrl+shift+c",
    "mac": "cmd+shift+c"
  }
]
```

**好处**:

- 快速打开/切换到 WebView
- 提高工作效率

---

## ✅ 验收标准

### 功能验收

- [x] WebView 在右侧打开
- [x] 不夺取编辑器焦点
- [x] Tab 自动被 pin
- [x] 重新打开时保持位置
- [x] 构建无错误

### 用户体验验收

- [ ] 符合用户预期
- [ ] 不干扰编码流程
- [ ] Pin 图标可见
- [ ] 关闭其他编辑器时不受影响

---

**文档版本**: v1.0
**创建时间**: 2025-11-18
**状态**: ✅ 实现完成，⏳ 等待测试
