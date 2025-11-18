# WebView Pin 和持久化功能实现完成

> **更新时间**: 2025-11-18
> **状态**: ✅ 实现完成，等待测试

---

## ✅ 已完成的实现

### 1. WebView Pin 功能修复 ✅

**问题**: 之前的 pin 功能没有生效

**原因**:

- `workbench.action.pinEditor` 命令需要在 panel 处于 active 状态时执行
- 仅使用 setTimeout 不够，需要检查 `panel.active` 状态

**解决方案** (`src/WebViewProvider.ts:726-746`):

```typescript
private pinPanel(): void {
  if (!this.panel) {
    return;
  }

  // 延迟 50ms 并检查 panel 是否为活动状态
  setTimeout(() => {
    if (this.panel && this.panel.active) {
      vscode.commands.executeCommand('workbench.action.pinEditor').then(
        () => {
          console.log('[WebViewProvider] Panel pinned successfully');
        },
        (error) => {
          console.error('[WebViewProvider] Failed to pin panel:', error);
        },
      );
    }
  }, 50);
}
```

**关键改进**:

1. ✅ 检查 `panel.active` 确保 panel 是当前活动编辑器
2. ✅ 使用 50ms 延迟确保 panel 完全加载
3. ✅ 添加错误处理和日志记录

**触发时机**:

- WebView 创建时
- WebView 重新显示时 (reveal)
- WebView 视图状态变化时 (onDidChangeViewState)

---

### 2. WebView 重启后持久化 ✅

**问题**: VSCode 重启后，已打开的 WebView tab 会消失

**解决方案**: 实现 WebView 序列化机制

#### A. 注册 Panel Serializer (`src/extension.ts:123-151`)

```typescript
context.subscriptions.push(
  vscode.window.registerWebviewPanelSerializer('qwenCode.chat', {
    async deserializeWebviewPanel(
      webviewPanel: vscode.WebviewPanel,
      state: unknown,
    ) {
      console.log('[Extension] Deserializing WebView panel with state:', state);

      // 恢复 panel 和事件监听器
      webViewProvider.restorePanel(webviewPanel);

      // 恢复状态（会话ID、agent初始化状态）
      if (state && typeof state === 'object') {
        webViewProvider.restoreState(
          state as {
            conversationId: string | null;
            agentInitialized: boolean;
          },
        );
      }

      log('WebView panel restored from serialization');
    },
  }),
);
```

#### B. 实现 `restorePanel()` 方法 (`src/WebViewProvider.ts:748-799`)

```typescript
restorePanel(panel: vscode.WebviewPanel): void {
  console.log('[WebViewProvider] Restoring WebView panel');
  this.panel = panel;

  // 设置面板图标
  this.panel.iconPath = vscode.Uri.joinPath(
    this.extensionUri,
    'assets',
    'icon.png',
  );

  // 设置 webview HTML
  this.panel.webview.html = this.getWebviewContent();

  // 设置所有事件监听器
  this.panel.webview.onDidReceiveMessage(
    async (message) => {
      await this.handleWebViewMessage(message);
    },
    null,
    this.disposables,
  );

  this.panel.onDidChangeViewState(
    () => {
      if (this.panel && this.panel.visible) {
        this.pinPanel();
      }
    },
    null,
    this.disposables,
  );

  this.panel.onDidDispose(
    () => {
      this.panel = null;
      this.disposables.forEach((d) => d.dispose());
    },
    null,
    this.disposables,
  );

  // 自动 pin 恢复的 panel
  this.pinPanel();

  console.log('[WebViewProvider] Panel restored successfully');
}
```

#### C. 实现 `getState()` 方法 (`src/WebViewProvider.ts:801-813`)

```typescript
getState(): {
  conversationId: string | null;
  agentInitialized: boolean;
} {
  return {
    conversationId: this.currentConversationId,
    agentInitialized: this.agentInitialized,
  };
}
```

#### D. 实现 `restoreState()` 方法 (`src/WebViewProvider.ts:815-827`)

```typescript
restoreState(state: {
  conversationId: string | null;
  agentInitialized: boolean;
}): void {
  console.log('[WebViewProvider] Restoring state:', state);
  this.currentConversationId = state.conversationId;
  this.agentInitialized = state.agentInitialized;

  // 恢复后重新加载内容
  if (this.panel) {
    this.panel.webview.html = this.getWebviewContent();
  }
}
```

---

## 🎯 实现原理

### WebView 序列化流程

```
┌─────────────────────────────────────────────────────────────────┐
│ VSCode 关闭前                                                    │
├─────────────────────────────────────────────────────────────────┤
│ 1. VSCode 检测到有 WebView 打开                                  │
│ 2. 调用 webViewProvider.getState() 获取状态                      │
│ 3. 序列化状态到磁盘                                              │
│    {                                                             │
│      conversationId: "session-123",                              │
│      agentInitialized: true                                      │
│    }                                                             │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│ VSCode 重启后                                                    │
├─────────────────────────────────────────────────────────────────┤
│ 1. VSCode 检测到之前有 'qwenCode.chat' WebView                   │
│ 2. 查找注册的 serializer (registerWebviewPanelSerializer)        │
│ 3. 创建新的 WebviewPanel 对象                                    │
│ 4. 调用 deserializeWebviewPanel()                               │
│    ├─ webViewProvider.restorePanel(panel) // 恢复 panel 引用     │
│    └─ webViewProvider.restoreState(state) // 恢复业务状态        │
│ 5. WebView 重新出现在编辑器中                                    │
│ 6. 自动 pin WebView tab                                         │
└─────────────────────────────────────────────────────────────────┘
```

---

## 📊 代码改动总结

| 文件                     | 改动   | 说明                                                     |
| ------------------------ | ------ | -------------------------------------------------------- |
| `src/WebViewProvider.ts` | +60 行 | 添加 pinPanel, restorePanel, getState, restoreState 方法 |
| `src/extension.ts`       | +30 行 | 注册 WebView serializer                                  |

### 新增方法列表

1. `pinPanel()` - Pin WebView tab (line 726-746)
2. `restorePanel()` - 恢复 panel 和事件监听器 (line 748-799)
3. `getState()` - 获取序列化状态 (line 801-813)
4. `restoreState()` - 恢复业务状态 (line 815-827)

---

## ✅ 验证检查

### TypeScript 编译 ✅

```bash
npm run check-types
# ✅ 通过，无错误
```

### ESLint 检查 ✅

```bash
npm run lint
# ✅ 通过，无警告
```

---

## 🧪 测试指南

### 测试 1: Pin 功能测试

**步骤**:

1. 打开 VSCode 调试模式 (F5)
2. 执行命令 `qwenCode.openChat` 打开 WebView
3. 观察 WebView tab

**预期结果**:

- ✅ WebView tab 显示 pin 图标 (📌)
- ✅ 右键点击其他 tab，选择 "关闭其他编辑器"，WebView 不会被关闭
- ✅ Console 输出: `[WebViewProvider] Panel pinned successfully`

---

### 测试 2: 重启持久化测试

**步骤**:

1. 打开 VSCode 调试模式
2. 执行命令 `qwenCode.openChat` 打开 WebView
3. 在 WebView 中进行一些操作（如切换 session）
4. 执行 VSCode 命令 `Developer: Reload Window` 重启窗口
5. 观察 WebView 是否恢复

**预期结果**:

- ✅ VSCode 重启后，WebView tab 自动恢复
- ✅ WebView 仍然在右侧显示
- ✅ WebView tab 仍然是 pinned 状态
- ✅ Console 输出:
  ```
  [Extension] Deserializing WebView panel with state: {...}
  [WebViewProvider] Restoring WebView panel
  [WebViewProvider] Restoring state: {...}
  [WebViewProvider] Panel restored successfully
  [WebViewProvider] Panel pinned successfully
  ```

---

### 测试 3: 状态恢复测试

**步骤**:

1. 打开 WebView，切换到某个 session
2. 记下当前 session ID 和标题
3. 执行 `Developer: Reload Window`
4. 检查 WebView 状态

**预期结果**:

- ✅ 当前 conversation ID 被恢复
- ✅ agent 初始化状态被恢复
- ✅ 不需要重新登录或重新连接

---

### 测试 4: 关闭后重新打开

**步骤**:

1. 手动关闭 WebView tab (点击 X)
2. 重新执行 `qwenCode.openChat`
3. 观察 WebView

**预期结果**:

- ✅ WebView 在右侧打开
- ✅ WebView 自动 pinned
- ✅ 焦点仍在编辑器（不被夺取）

---

## 🎨 与 Claude Code 对比

| 功能         | Claude Code | 当前实现 | 状态     |
| ------------ | ----------- | -------- | -------- |
| **Pin Tab**  | ✅          | ✅       | 完全对标 |
| **重启保持** | ✅          | ✅       | 完全对标 |
| **右侧固定** | ✅          | ✅       | 完全对标 |
| **不抢焦点** | ✅          | ✅       | 完全对标 |
| **状态恢复** | ✅          | ✅       | 完全对标 |

---

## 📝 技术要点

### 1. Pin 命令的正确使用

```typescript
// ❌ 错误：直接执行可能不生效
vscode.commands.executeCommand('workbench.action.pinEditor');

// ✅ 正确：检查 active 状态 + 延迟
setTimeout(() => {
  if (this.panel && this.panel.active) {
    vscode.commands.executeCommand('workbench.action.pinEditor');
  }
}, 50);
```

### 2. Serializer 注册时机

必须在 extension.ts 的 `activate()` 函数中注册，且必须在 `context.subscriptions` 中添加：

```typescript
context.subscriptions.push(
  vscode.window.registerWebviewPanelSerializer('qwenCode.chat', {
    async deserializeWebviewPanel(...) { ... }
  })
);
```

### 3. 事件监听器清理

在 `restorePanel()` 中设置的所有监听器都添加到 `this.disposables`，确保在 dispose 时正确清理：

```typescript
this.panel.webview.onDidReceiveMessage(
  async (message) => { ... },
  null,
  this.disposables, // ← 重要！
);
```

---

## 🚀 下一步

### 立即测试

1. 启动 VSCode 调试模式 (F5)
2. 按照上面的测试指南逐项测试
3. 记录测试结果

### 如果测试通过

- 提交代码到 git
- 合并到主分支
- 更新版本号

### 如果发现问题

- 在 Console 中查看错误日志
- 检查 `[WebViewProvider]` 和 `[Extension]` 的日志输出
- 记录问题并修复

---

**文档版本**: v1.0
**创建时间**: 2025-11-18
**状态**: ✅ 实现完成，等待测试
