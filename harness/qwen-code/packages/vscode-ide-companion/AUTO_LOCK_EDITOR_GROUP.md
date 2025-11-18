# 自动锁定编辑器组功能实现

## 概述

参考 Claude Code 的实现，Qwen Code VSCode 扩展现在支持自动锁定编辑器组功能，确保 AI 助手界面保持稳定，不会被其他编辑器替换或意外关闭。

## 实现原理

### 1. VSCode 锁定组机制

**VSCode 源码分析**（`src/vs/workbench/browser/parts/editor/editor.contribution.ts:558-566`）：

```typescript
// Lock Group: only on auxiliary window and when group is unlocked
appendEditorToolItem(
  {
    id: LOCK_GROUP_COMMAND_ID,
    title: localize('lockEditorGroup', 'Lock Group'),
    icon: Codicon.unlock,
  },
  ContextKeyExpr.and(
    IsAuxiliaryEditorPartContext,
    ActiveEditorGroupLockedContext.toNegated(),
  ),
  CLOSE_ORDER - 1, // immediately to the left of close action
);
```

**关键条件**：

- `IsAuxiliaryEditorPartContext`: 当前是辅助窗口的编辑器组
- `ActiveEditorGroupLockedContext.toNegated()`: 当前组未锁定

### 2. Claude Code 的实现方式

Claude Code 在创建 webview panel 时会检测是否在新列中打开：

```typescript
context.subscriptions.push(
  vscode.commands.registerCommand(
    'claude-vscode.editor.open',
    async (param1, param2) => {
      context.globalState.update('lastClaudeLocation', 1);
      let { startedInNewColumn } = webviewProvider.createPanel(param1, param2);

      // 如果在新列中打开，则自动锁定编辑器组
      if (startedInNewColumn) {
        await vscode.commands.executeCommand(
          'workbench.action.lockEditorGroup',
        );
      }
    },
  ),
);
```

### 3. Qwen Code 的实现

**文件位置**: `packages/vscode-ide-companion/src/WebViewProvider.ts:101-153`

```typescript
async show(): Promise<void> {
  // Track if we're creating a new panel in a new column
  let startedInNewColumn = false;

  if (this.panel) {
    // If panel already exists, just reveal it (no lock needed)
    this.revealPanelTab(true);
    this.capturePanelTab();
    return;
  }

  // Mark that we're creating a new panel
  startedInNewColumn = true;

  this.panel = vscode.window.createWebviewPanel(
    'qwenCode.chat',
    'Qwen Code Chat',
    {
      viewColumn: vscode.ViewColumn.Beside, // Open on right side of active editor
      preserveFocus: true, // Don't steal focus from editor
    },
    {
      enableScripts: true,
      retainContextWhenHidden: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'dist')],
    },
  );

  // Capture the Tab that corresponds to our WebviewPanel (Claude-style)
  this.capturePanelTab();

  // Auto-lock editor group when opened in new column (Claude Code style)
  if (startedInNewColumn) {
    console.log('[WebViewProvider] Auto-locking editor group for Qwen Code chat');
    try {
      // Reveal panel without preserving focus to make it the active group
      // This ensures the lock command targets the correct editor group
      this.revealPanelTab(false);

      await vscode.commands.executeCommand('workbench.action.lockEditorGroup');
      console.log('[WebViewProvider] Editor group locked successfully');
    } catch (error) {
      console.warn('[WebViewProvider] Failed to lock editor group:', error);
      // Non-fatal error, continue anyway
    }
  } else {
    // For existing panel, reveal with preserving focus
    this.revealPanelTab(true);
  }

  // Continue with panel setup...
}
```

### 关键修复：preserveFocus 问题

**问题发现**：

- 最初实现中，`createWebviewPanel` 使用了 `preserveFocus: true`
- 这导致焦点保留在左边的编辑器组，左边的组仍然是**活动组（activeGroup）**
- 执行 `workbench.action.lockEditorGroup` 时，命令默认作用于活动组
- 结果：**错误地锁定了左边的编辑器组**，而不是 webview 所在的组

**错误的执行流程**：

```
1. createWebviewPanel() 创建新组
   └─> preserveFocus: true 保持焦点在左边
       └─> activeGroup 仍然是左边的编辑器组

2. executeCommand("workbench.action.lockEditorGroup")
   └─> resolveCommandsContext() 使用 activeGroup
       └─> activeGroup = 左边的编辑器组 ❌
           └─> 错误地锁定了左边的组
```

**修复方案**：

1. 在执行锁定命令之前，调用 `this.revealPanelTab(false)`
2. 这会让 webview panel 获得焦点并成为活动组
3. 然后执行锁定命令就会锁定正确的组

**修复后的执行流程**：

```
1. createWebviewPanel() 创建新组
   └─> preserveFocus: true 保持焦点在左边

2. revealPanelTab(false) 激活 webview 组
   └─> webview 组成为 activeGroup

3. executeCommand("workbench.action.lockEditorGroup")
   └─> resolveCommandsContext() 使用 activeGroup
       └─> activeGroup = webview 所在的组 ✓
           └─> 正确锁定 webview 所在的组
```

## 执行流程

```
1. 用户打开 Qwen Code chat
   ↓
2. 调用 WebViewProvider.show()
   ↓
3. 检查是否已有 panel
   - 有：直接 reveal，不执行锁定
   - 无：创建新 panel，设置 startedInNewColumn = true
   ↓
4. 创建 webview panel
   - viewColumn: ViewColumn.Beside
   - preserveFocus: true (不抢夺焦点，保持在编辑器)
   ↓
5. 捕获 Tab 引用
   - 调用 capturePanelTab() 保存 Tab 对象
   ↓
6. 执行自动锁定（startedInNewColumn === true）
   - 调用 revealPanelTab(false) 激活 webview 组
   - webview 所在的组成为活动组（activeGroup）
   - 执行命令: workbench.action.lockEditorGroup
   - 命令作用于活动组，正确锁定 webview 组
   ↓
7. 编辑器组被锁定
   - ActiveEditorGroupLockedContext 变为 true
   - 工具栏显示"解锁组"按钮（锁定图标）
   - webview 保持在固定位置
```

## 功能效果

### 锁定前

- ❌ 用户可以拖拽 Qwen Code panel 到其他位置
- ❌ 其他编辑器可能替换 Qwen Code panel
- ❌ 容易意外关闭整个编辑器组

### 锁定后

- ✅ Qwen Code panel 保持在固定位置
- ✅ 编辑器组不会被其他操作影响
- ✅ 工具栏显示"锁定组"按钮，用户可以手动解锁
- ✅ 类似侧边栏的稳定行为

## 设计优势

1. **防止意外操作**
   - 锁定后用户不能轻易拖拽或关闭 AI 助手界面
   - 减少误操作导致的工作流中断

2. **保持固定位置**
   - AI 助手界面始终在用户期望的位置
   - 符合"AI 助手作为辅助工具"的定位

3. **用户可控**
   - 自动锁定提供默认保护
   - 用户仍可以通过工具栏解锁按钮手动解锁
   - 平衡了便利性和灵活性

4. **一致的用户体验**
   - 与 Claude Code 保持一致的交互模式
   - 用户无需学习新的行为模式

## 错误处理

```typescript
try {
  await vscode.commands.executeCommand('workbench.action.lockEditorGroup');
  console.log('[WebViewProvider] Editor group locked successfully');
} catch (error) {
  console.warn('[WebViewProvider] Failed to lock editor group:', error);
  // Non-fatal error, continue anyway
}
```

**设计考虑**：

- 锁定失败不影响 panel 的正常功能
- 记录警告日志便于调试
- 优雅降级，不中断用户工作流

## 配置选项（可选扩展）

如果需要让用户控制是否自动锁定，可以添加配置项：

```typescript
// 在 package.json 中添加配置
"qwenCode.autoLockEditorGroup": {
  "type": "boolean",
  "default": true,
  "description": "Automatically lock the editor group when opening Qwen Code chat"
}

// 在代码中检查配置
const config = vscode.workspace.getConfiguration('qwenCode');
const autoLock = config.get<boolean>('autoLockEditorGroup', true);

if (startedInNewColumn && autoLock) {
  await vscode.commands.executeCommand('workbench.action.lockEditorGroup');
}
```

## 测试场景

### 场景 1: 首次打开 Qwen Code

1. 打开 VSCode，没有 Qwen Code panel
2. 执行命令打开 Qwen Code chat
3. **预期**: Panel 在新列中打开，编辑器组自动锁定

### 场景 2: 已有 Qwen Code panel

1. Qwen Code panel 已打开
2. 切换到其他编辑器
3. 再次打开 Qwen Code chat
4. **预期**: Panel 被 reveal，不重复锁定

### 场景 3: 手动解锁后

1. Qwen Code panel 已锁定
2. 用户点击工具栏解锁按钮
3. 编辑器组被解锁
4. **预期**: 用户可以自由操作编辑器组

### 场景 4: 关闭并重新打开

1. Qwen Code panel 已打开并锁定
2. 用户关闭 panel
3. 再次打开 Qwen Code chat
4. **预期**: 新 panel 在新列打开，自动锁定

## 兼容性

- ✅ VSCode 1.85+（支持 `workbench.action.lockEditorGroup` 命令）
- ✅ 所有操作系统（Windows, macOS, Linux）
- ✅ 不影响现有功能
- ✅ 向后兼容旧版本 VSCode（锁定失败时优雅降级）

## 相关 VSCode 命令

| 命令                                     | 功能                 |
| ---------------------------------------- | -------------------- |
| `workbench.action.lockEditorGroup`       | 锁定当前编辑器组     |
| `workbench.action.unlockEditorGroup`     | 解锁当前编辑器组     |
| `workbench.action.toggleEditorGroupLock` | 切换编辑器组锁定状态 |

## 总结

通过模仿 Claude Code 的实现，Qwen Code 现在提供了：

1. ✅ 自动锁定编辑器组功能
2. ✅ 与 Claude Code 一致的用户体验
3. ✅ 稳定的 AI 助手界面位置
4. ✅ 优雅的错误处理

这个功能显著提升了用户体验，让 AI 助手界面更加稳定可靠！
