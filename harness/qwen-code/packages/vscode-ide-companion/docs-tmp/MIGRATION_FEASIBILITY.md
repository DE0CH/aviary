# Claude Code VSCode 扩展功能迁移可行性分析

## 一、概述

### 参考插件信息

- **名称**: Claude Code for VS Code (Anthropic 官方)
- **版本**: 2.0.43
- **状态**: 已打包压缩 (extension.js 约 983KB)

### 目标插件信息

- **名称**: Qwen Code VSCode IDE Companion
- **版本**: 0.2.2
- **状态**: 源代码可用,架构清晰

---

## 二、需求功能分析

### 用户期望迁移的功能

#### 1. WebView CustomEditor 固定在编辑器右侧

**描述**: 将 webview 面板默认显示在代码编辑器的右侧(split view)

**当前状态**:

- Qwen 扩展: WebView 使用 `vscode.ViewColumn.One` (主编辑器列)
- Claude 扩展: 支持多种布局方式

**可行性**: ✅ **完全可行**

**实现方案**:

```typescript
// 当前实现 (WebViewProvider.ts:77)
this.panel = vscode.window.createWebviewPanel(
  'qwenCode.chat',
  'Qwen Code Chat',
  vscode.ViewColumn.One, // ← 修改这里
  {
    /* ... */
  },
);

// 建议修改为
this.panel = vscode.window.createWebviewPanel(
  'qwenCode.chat',
  'Qwen Code Chat',
  vscode.ViewColumn.Beside, // 在当前编辑器右侧打开
  {
    /* ... */
  },
);
```

**附加选项**:

- `vscode.ViewColumn.Beside`: 在当前活动编辑器旁边
- `vscode.ViewColumn.Two`: 固定在第二列
- 可配置化,让用户选择默认位置

#### 2. Webview 顶部组件布局

##### 2.1 左侧: Session/Chat 选择器 (下拉菜单)

**描述**: 顶部左侧显示当前 session 名称,点击可下拉选择其他 session

**当前状态**:

- Qwen 扩展: 右侧有 "📋 Sessions" 按钮,点击打开模态框
- Claude 扩展: CSS 显示有 `.E` 类(下拉按钮样式)

**可行性**: ✅ **完全可行**

**实现方案**:

**方案 A: 移动现有按钮到左侧**

```tsx
// App.tsx - 修改 header 布局
<div className="chat-header">
  {/* 新增:左侧 session 选择器 */}
  <div className="session-selector-dropdown">
    <button
      className="session-dropdown-button"
      onClick={handleLoadQwenSessions}
    >
      <span className="session-icon">📋</span>
      <span className="session-title">
        {currentSessionTitle || 'Select Session'}
      </span>
      <span className="dropdown-icon">▼</span>
    </button>
  </div>

  {/* 右侧新建 chat 按钮 */}
  <div className="header-actions">
    <button className="new-chat-button" onClick={handleNewQwenSession}>
      ➕
    </button>
  </div>
</div>
```

**方案 B: 使用真正的下拉选择**

```tsx
// 使用 VSCode 原生选择器样式
<select
  className="session-selector"
  value={currentSessionId}
  onChange={(e) => handleSwitchSession(e.target.value)}
>
  {qwenSessions.map((session) => (
    <option key={session.id} value={session.id}>
      {getSessionTitle(session)}
    </option>
  ))}
</select>
```

**CSS 样式**:

```css
/* App.css - 添加以下样式 */
.chat-header {
  display: flex;
  justify-content: space-between; /* 两端对齐 */
  align-items: center;
  padding: 12px 16px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.session-selector-dropdown {
  flex: 1;
  min-width: 0;
}

.session-dropdown-button {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  background: transparent;
  border: 1px solid var(--vscode-input-border);
  border-radius: 4px;
  color: var(--vscode-foreground);
  cursor: pointer;
  max-width: 300px;
  overflow: hidden;
}

.session-dropdown-button:hover {
  background: var(--vscode-toolbar-hoverBackground);
}

.session-title {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  text-align: left;
}

.dropdown-icon {
  flex-shrink: 0;
  opacity: 0.7;
}
```

##### 2.2 右侧: 新建 Chat 按钮 (+ 号)

**描述**: 顶部右上角显示 + 号按钮,点击创建新 chat

**当前状态**:

- Qwen 扩展: 新建按钮在 session 选择器模态框内
- Claude 扩展: CSS 显示有 `.j` 类(图标按钮样式)

**可行性**: ✅ **完全可行**

**实现方案**:

```tsx
// App.tsx - Header 右侧按钮
<div className="header-actions">
  <button
    className="icon-button new-chat-button"
    onClick={handleNewQwenSession}
    title="New Chat"
  >
    <svg width="16" height="16" viewBox="0 0 16 16">
      <path d="M8 1v14M1 8h14" stroke="currentColor" strokeWidth="2" />
    </svg>
  </button>
</div>
```

**CSS 样式**:

```css
.header-actions {
  display: flex;
  align-items: center;
  gap: 4px;
}

.icon-button {
  width: 24px;
  height: 24px;
  padding: 4px;
  background: transparent;
  border: none;
  border-radius: 4px;
  color: var(--vscode-foreground);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.icon-button:hover {
  background: var(--vscode-toolbar-hoverBackground);
}

.icon-button:active {
  opacity: 0.7;
}
```

---

## 三、关键差异分析

### Claude Code 扩展的特点

#### 1. 多种打开方式

```json
{
  "commands": [
    { "command": "claude-vscode.editor.open", "title": "Open in New Tab" },
    { "command": "claude-vscode.sidebar.open", "title": "Open in Side Bar" },
    { "command": "claude-vscode.window.open", "title": "Open in New Window" }
  ]
}
```

**迁移建议**:

- 保留 Qwen 扩展的简单模式(单一命令)
- 可选:后续添加多种打开方式的支持

#### 2. Sidebar View Container

```json
{
  "viewsContainers": {
    "activitybar": [
      {
        "id": "claude-sidebar",
        "title": "Claude",
        "icon": "resources/claude-logo.svg"
      }
    ]
  },
  "views": {
    "claude-sidebar": [
      {
        "type": "webview",
        "id": "claudeVSCodeSidebar",
        "name": "Claude Code"
      }
    ]
  }
}
```

**迁移建议**:

- Qwen 扩展暂时不需要 Sidebar 容器
- 当前的 WebView Panel 方式更灵活

#### 3. 配置项差异

| 配置项   | Claude Code             | Qwen Code    | 迁移建议 |
| -------- | ----------------------- | ------------ | -------- |
| 模型选择 | `selectedModel`         | `qwen.model` | 保持现有 |
| 环境变量 | `environmentVariables`  | 无           | 可选添加 |
| 终端模式 | `useTerminal`           | 无           | 不需要   |
| 权限模式 | `initialPermissionMode` | 无           | 不需要   |

---

## 四、实现步骤建议

### 阶段一: 基础布局调整 (1-2 天)

#### 任务 1: 修改 WebView 打开位置

**文件**: `src/WebViewProvider.ts`

```typescript
// 修改 show() 方法
async show(): Promise<void> {
  if (this.panel) {
    this.panel.reveal();
    return;
  }

  this.panel = vscode.window.createWebviewPanel(
    'qwenCode.chat',
    'Qwen Code Chat',
    {
      viewColumn: vscode.ViewColumn.Beside,  // 新增配置
      preserveFocus: false
    },
    {
      enableScripts: true,
      retainContextWhenHidden: true,
      localResourceRoots: [
        vscode.Uri.joinPath(this.extensionUri, 'dist')
      ],
    },
  );

  // ... 其余代码
}
```

#### 任务 2: 重构 Header 组件

**文件**: `src/webview/App.tsx`

```tsx
// 新增组件:ChatHeader
const ChatHeader: React.FC<{
  currentSessionTitle: string;
  onSessionsClick: () => void;
  onNewChatClick: () => void;
}> = ({ currentSessionTitle, onSessionsClick, onNewChatClick }) => {
  return (
    <div className="chat-header">
      <div className="session-selector-container">
        <button className="session-dropdown-button" onClick={onSessionsClick}>
          <span className="session-icon">📋</span>
          <span className="session-title">
            {currentSessionTitle || 'Select Session'}
          </span>
          <span className="dropdown-icon">▼</span>
        </button>
      </div>

      <div className="header-actions">
        <button
          className="icon-button new-chat-button"
          onClick={onNewChatClick}
          title="New Chat (Ctrl+Shift+N)"
        >
          <svg width="16" height="16" viewBox="0 0 16 16">
            <path
              d="M8 1v14M1 8h14"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </div>
    </div>
  );
};

// 在 App 组件中使用
export const App: React.FC = () => {
  const [currentSessionTitle, setCurrentSessionTitle] = useState<string>('');

  // ... 其他状态

  return (
    <div className="chat-container">
      <ChatHeader
        currentSessionTitle={currentSessionTitle}
        onSessionsClick={handleLoadQwenSessions}
        onNewChatClick={handleNewQwenSession}
      />

      {/* 其余组件 */}
    </div>
  );
};
```

#### 任务 3: 更新样式

**文件**: `src/webview/App.css`

```css
/* 替换现有的 .chat-header 样式 */
.chat-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 12px;
  background-color: var(--vscode-editor-background);
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  min-height: 40px;
}

.session-selector-container {
  flex: 1;
  min-width: 0;
  margin-right: 12px;
}

.session-dropdown-button {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  background: transparent;
  border: 1px solid var(--vscode-input-border);
  border-radius: 4px;
  color: var(--vscode-foreground);
  cursor: pointer;
  max-width: 100%;
  overflow: hidden;
  transition: background-color 0.2s;
}

.session-dropdown-button:hover {
  background: var(--vscode-toolbar-hoverBackground);
}

.session-dropdown-button:active {
  background: var(--vscode-list-activeSelectionBackground);
}

.session-icon {
  flex-shrink: 0;
  font-size: 14px;
}

.session-title {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  text-align: left;
  font-size: 13px;
  font-weight: 500;
}

.dropdown-icon {
  flex-shrink: 0;
  opacity: 0.7;
  font-size: 10px;
  transition: transform 0.2s;
}

.session-dropdown-button[aria-expanded='true'] .dropdown-icon {
  transform: rotate(180deg);
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}

.icon-button {
  width: 28px;
  height: 28px;
  padding: 0;
  background: transparent;
  border: none;
  border-radius: 4px;
  color: var(--vscode-foreground);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background-color 0.2s;
}

.icon-button:hover {
  background: var(--vscode-toolbar-hoverBackground);
}

.icon-button:active {
  opacity: 0.7;
}

.new-chat-button svg {
  width: 16px;
  height: 16px;
}

/* 移除或修改原有的 .session-button 样式 */
.session-button {
  /* 已移除,功能整合到 header */
}
```

### 阶段二: 功能增强 (2-3 天)

#### 任务 4: 添加当前 Session 显示逻辑

```typescript
// WebViewProvider.ts - 添加方法
private currentSessionId: string | null = null;
private currentSessionTitle: string = '';

private async updateCurrentSessionInfo(sessionId: string): Promise<void> {
  try {
    const sessions = await this.agentManager.getSessionList();
    const currentSession = sessions.find(s =>
      (s.id === sessionId || s.sessionId === sessionId)
    );

    if (currentSession) {
      const title = this.getSessionTitle(currentSession);
      this.currentSessionTitle = title;
      this.sendMessageToWebView({
        type: 'currentSessionUpdated',
        data: { sessionId, title }
      });
    }
  } catch (error) {
    console.error('Failed to update session info:', error);
  }
}

private getSessionTitle(session: Record<string, unknown>): string {
  const title = session.title || session.name;
  if (title) return title as string;

  // 从第一条消息提取标题
  const messages = session.messages as Array<any> || [];
  const firstUserMessage = messages.find(m => m.type === 'user');
  if (firstUserMessage && firstUserMessage.content) {
    return firstUserMessage.content.substring(0, 50) + '...';
  }

  return 'Untitled Session';
}
```

```tsx
// App.tsx - 添加消息处理
useEffect(() => {
  const messageHandler = (event: MessageEvent) => {
    const message = event.data;

    switch (message.type) {
      case 'currentSessionUpdated':
        setCurrentSessionTitle(message.data.title);
        break;
      // ... 其他 case
    }
  };

  window.addEventListener('message', messageHandler);
  return () => window.removeEventListener('message', messageHandler);
}, []);
```

#### 任务 5: 添加键盘快捷键支持

**文件**: `package.json`

```json
{
  "contributes": {
    "keybindings": [
      {
        "command": "qwenCode.openChat",
        "key": "ctrl+shift+a",
        "mac": "cmd+shift+a"
      },
      {
        "command": "qwenCode.newSession",
        "key": "ctrl+shift+n",
        "mac": "cmd+shift+n",
        "when": "qwenCode.chatVisible"
      }
    ]
  }
}
```

**文件**: `src/extension.ts`

```typescript
context.subscriptions.push(
  vscode.commands.registerCommand('qwenCode.newSession', async () => {
    await webViewProvider.createNewSession();
  }),
);
```

### 阶段三: 优化和测试 (1-2 天)

#### 任务 6: Session 切换动画

```css
/* App.css - 添加过渡动画 */
.messages-container {
  transition: opacity 0.2s ease-in-out;
}

.messages-container.switching {
  opacity: 0.5;
}

@keyframes fadeIn {
  from {
    opacity: 0;
    transform: translateY(10px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.message {
  animation: fadeIn 0.3s ease-out;
}
```

#### 任务 7: 下拉菜单优化

**方案 A: 简单下拉(当前模态框改为下拉)**

```tsx
// 将 session-selector-overlay 改为相对定位的下拉菜单
<div className="session-dropdown" ref={dropdownRef}>
  {showSessionSelector && (
    <div className="session-dropdown-menu">
      <div className="session-dropdown-header">
        <span>Recent Sessions</span>
        <button onClick={handleNewQwenSession}>➕ New</button>
      </div>
      <div className="session-dropdown-list">
        {qwenSessions.map((session) => (
          <div
            key={session.id}
            className="session-dropdown-item"
            onClick={() => handleSwitchSession(session.id)}
          >
            <div className="session-item-title">{getTitle(session)}</div>
            <div className="session-item-meta">
              {getTimeAgo(session.lastUpdated)}
            </div>
          </div>
        ))}
      </div>
    </div>
  )}
</div>
```

```css
.session-dropdown {
  position: relative;
}

.session-dropdown-menu {
  position: absolute;
  top: 100%;
  left: 0;
  margin-top: 4px;
  min-width: 300px;
  max-width: 400px;
  max-height: 400px;
  background-color: var(--vscode-menu-background);
  border: 1px solid var(--vscode-menu-border);
  border-radius: 4px;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
  z-index: 1000;
  overflow: hidden;
  animation: dropdownSlideIn 0.2s ease-out;
}

@keyframes dropdownSlideIn {
  from {
    opacity: 0;
    transform: translateY(-10px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.session-dropdown-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 12px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  font-weight: 600;
}

.session-dropdown-list {
  max-height: 350px;
  overflow-y: auto;
  padding: 4px;
}

.session-dropdown-item {
  padding: 8px 12px;
  border-radius: 4px;
  cursor: pointer;
  transition: background-color 0.2s;
}

.session-dropdown-item:hover {
  background-color: var(--vscode-list-hoverBackground);
}

.session-dropdown-item.active {
  background-color: var(--vscode-list-activeSelectionBackground);
  color: var(--vscode-list-activeSelectionForeground);
}

.session-item-title {
  font-size: 13px;
  margin-bottom: 2px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.session-item-meta {
  font-size: 11px;
  opacity: 0.7;
}
```

---

## 五、风险评估

### 低风险 ✅

1. **WebView 位置调整**: 只需修改一个参数
2. **Header 布局重构**: 不影响现有功能,纯 UI 调整
3. **CSS 样式添加**: 增量修改,不破坏现有样式

### 中风险 ⚠️

1. **Session 标题提取逻辑**: 需要处理多种数据格式
   - **缓解措施**: 添加完善的 fallback 逻辑

2. **下拉菜单点击外部关闭**: 需要添加事件监听
   - **缓解措施**: 使用 React hooks (useEffect + useRef)

### 无高风险项

---

## 六、测试计划

### 单元测试

- [ ] Session 标题提取函数测试
- [ ] Session 列表过滤和排序测试

### 集成测试

- [ ] WebView 打开位置验证
- [ ] Session 切换流程测试
- [ ] 新建 Chat 功能测试

### 用户体验测试

- [ ] 不同窗口布局下的显示效果
- [ ] 键盘快捷键功能
- [ ] 长 Session 标题的显示
- [ ] 主题切换(Light/Dark/High Contrast)

### 性能测试

- [ ] 大量 Session 列表渲染性能
- [ ] Session 切换动画流畅度

---

## 七、最终建议

### ✅ 推荐迁移的功能

1. **WebView 固定右侧**: 简单且用户体验提升明显
2. **Header 重构**:
   - 左侧 Session 选择器
   - 右侧新建按钮
3. **下拉菜单样式**: 比模态框更符合 IDE 操作习惯

### ⏸️ 建议延后的功能

1. **多种打开方式**(Editor/Sidebar/Window): 当前单一方式已足够
2. **Terminal 模式**: Qwen 不需要此功能
3. **复杂权限管理**: 当前实现已满足需求

### 📋 实现优先级

#### P0 (核心功能,必须实现)

1. WebView 打开在右侧列
2. Header 组件重构(左侧 session,右侧新建)
3. 当前 Session 标题显示

#### P1 (重要优化)

1. 下拉菜单替代模态框
2. 键盘快捷键支持
3. Session 切换动画

#### P2 (可选增强)

1. Session 搜索功能
2. Session 固定/收藏
3. 最近使用 Session 快速切换

---

## 八、时间估算

| 阶段            | 工作量     | 说明                             |
| --------------- | ---------- | -------------------------------- |
| 阶段一:基础布局 | 1-2 天     | WebView 位置 + Header 重构 + CSS |
| 阶段二:功能增强 | 2-3 天     | Session 显示 + 快捷键 + 优化     |
| 阶段三:测试调优 | 1-2 天     | 测试 + Bug 修复 + 文档           |
| **总计**        | **4-7 天** | 取决于测试覆盖范围               |

---

## 九、结论

### 可行性评估: ✅ **高度可行**

1. **技术可行性**: 100%
   - 所需功能均在 VSCode API 支持范围内
   - 现有架构完全支持
   - 无需引入新的依赖

2. **实现复杂度**: 低到中等
   - 核心改动量小
   - 主要是 UI/UX 调整
   - 不涉及底层协议变更

3. **迁移风险**: 低
   - 不影响现有核心功能
   - 改动均为增量式
   - 易于回滚

### 推荐行动方案

#### 立即可做 (Quick Win)

```bash
# 1. 修改 WebView 打开位置
# src/WebViewProvider.ts:77
vscode.ViewColumn.Beside

# 2. 重构 Header 布局
# 预计 2-3 小时即可完成基础版本
```

#### 短期优化 (1 周内)

- 完整实现 P0 功能
- 添加基础测试
- 文档更新

#### 长期规划 (后续迭代)

- P1/P2 功能根据用户反馈逐步添加
- 性能优化和细节打磨

---

## 附录: 参考代码片段

### A. 点击外部关闭下拉菜单

```tsx
const useClickOutside = (
  ref: React.RefObject<HTMLElement>,
  handler: () => void,
) => {
  useEffect(() => {
    const listener = (event: MouseEvent | TouchEvent) => {
      if (!ref.current || ref.current.contains(event.target as Node)) {
        return;
      }
      handler();
    };

    document.addEventListener('mousedown', listener);
    document.addEventListener('touchstart', listener);

    return () => {
      document.removeEventListener('mousedown', listener);
      document.removeEventListener('touchstart', listener);
    };
  }, [ref, handler]);
};

// 使用
const dropdownRef = useRef<HTMLDivElement>(null);
useClickOutside(dropdownRef, () => setShowSessionSelector(false));
```

### B. Session 时间格式化

```typescript
function getTimeAgo(timestamp: string | number): string {
  const now = Date.now();
  const time =
    typeof timestamp === 'string' ? new Date(timestamp).getTime() : timestamp;
  const diff = now - time;

  const minutes = Math.floor(diff / 60000);
  const hours = Math.floor(diff / 3600000);
  const days = Math.floor(diff / 86400000);

  if (minutes < 1) return 'Just now';
  if (minutes < 60) return `${minutes}m ago`;
  if (hours < 24) return `${hours}h ago`;
  if (days < 7) return `${days}d ago`;

  return new Date(time).toLocaleDateString();
}
```

### C. 渐进式实现策略

```typescript
// Phase 1: 简单移动
const Header = () => (
  <div className="chat-header">
    <button onClick={onSessions}>Sessions ▼</button>
    <button onClick={onNew}>➕</button>
  </div>
);

// Phase 2: 显示当前 Session
const Header = ({ currentSession }) => (
  <div className="chat-header">
    <button onClick={onSessions}>
      {currentSession?.title || 'Select Session'} ▼
    </button>
    <button onClick={onNew}>➕</button>
  </div>
);

// Phase 3: 完整下拉菜单
const Header = ({ currentSession, sessions }) => (
  <div className="chat-header">
    <Dropdown
      current={currentSession}
      items={sessions}
      onCreate={onNew}
    />
    <button onClick={onNew}>➕</button>
  </div>
);
```

---

**文档版本**: v1.0
**创建日期**: 2025-11-18
**作者**: Claude (Sonnet 4.5)
**审核状态**: 待审核
