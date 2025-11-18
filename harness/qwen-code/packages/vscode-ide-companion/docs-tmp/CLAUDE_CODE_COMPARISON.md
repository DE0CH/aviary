# AionUI VSCode 插件 vs Claude Code 功能对比

> **对标目标**：本文档以 Claude Code 为对标基准，详细分析功能差异并制定实现计划。

## 目录

- [Claude Code 核心功能](#claude-code-核心功能)
- [功能对比矩阵](#功能对比矩阵)
- [实现优先级规划](#实现优先级规划)
- [技术实现方案](#技术实现方案)

---

## Claude Code 核心功能

### 1. **聊天界面** (Chat Interface)

**功能描述**：

- 专用的侧边栏面板
- 实时显示 AI 响应
- 支持流式输出
- 消息历史记录

**当前状态**：

- ✅ 已实现：WebView 聊天界面
- ✅ 已实现：流式响应
- ✅ 已实现：会话历史

**差距**：

- ⚠️ UI 美观度可以优化
- ⚠️ 缺少侧边栏集成（目前是独立面板）

### 2. **内联 Diff 预览** (Inline Diffs)

**功能描述**：

```
Claude 修改代码时：
1. 自动打开 VSCode 原生 diff 视图
2. 并排显示修改前后代码
3. 可以一键接受/拒绝修改
```

**当前状态**：

- ❌ 未实现

**实现难度**：⭐⭐⭐（中等）

**实现方案**：

```typescript
// 当 Agent 请求文件修改时
case 'fs/write_text_file':
  const oldContent = fs.readFileSync(path);
  const newContent = params.content;

  // 打开 diff 视图
  await vscode.commands.executeCommand('vscode.diff',
    vscode.Uri.parse(`untitled:${path}?old`).with({ query: oldContent }),
    vscode.Uri.parse(`untitled:${path}?new`).with({ query: newContent }),
    `${path} (AI Changes)`
  );

  // 等待用户确认
  const accept = await vscode.window.showQuickPick(['Accept', 'Reject']);
  if (accept === 'Accept') {
    fs.writeFileSync(path, newContent);
  }
```

### 3. **文件引用** (@-mention files)

**功能描述**：

```
用户输入：
  "请优化 @src/App.tsx 的性能"

系统行为：
  1. 解析 @src/App.tsx
  2. 读取文件内容
  3. 自动添加到上下文
```

**当前状态**：

- ❌ 未实现

**实现难度**：⭐⭐（简单）

**实现方案**：

```typescript
// 1. 解析用户输入
function parseFileReferences(message: string): {
  files: string[];
  cleanMessage: string;
} {
  const filePattern = /@([\w\/\.\-]+)/g;
  const files = [];
  let match;

  while ((match = filePattern.exec(message)) !== null) {
    files.push(match[1]);
  }

  const cleanMessage = message.replace(filePattern, (_, file) => file);
  return { files, cleanMessage };
}

// 2. 读取文件内容
async function injectFileContext(message: string): Promise<string> {
  const { files, cleanMessage } = parseFileReferences(message);

  if (files.length === 0) return message;

  let context = '';
  for (const file of files) {
    const content = await vscode.workspace.fs.readFile(
      vscode.Uri.file(workspaceRoot + '/' + file),
    );
    context += `\n\n[File: ${file}]\n\`\`\`\n${content}\n\`\`\`\n`;
  }

  return context + '\n\nUser: ' + cleanMessage;
}
```

### 4. **自动上下文感知** (Context Awareness)

**功能描述**：

```
自动检测并注入：
- 当前打开的文件
- 选中的代码
- 光标位置
- 工作区路径
```

**当前状态**：

- ❌ 未实现

**实现难度**：⭐（非常简单）

**实现方案**：

```typescript
async function collectVSCodeContext(): Promise<string> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return '';

  const document = editor.document;
  const selection = editor.selection;

  let context = '[VSCode Context]\n';

  // 当前文件
  context += `File: ${document.fileName}\n`;
  context += `Language: ${document.languageId}\n`;

  // 选中的代码
  if (!selection.isEmpty) {
    const selectedText = document.getText(selection);
    context += `\nSelected Code (lines ${selection.start.line + 1}-${selection.end.line + 1}):\n`;
    context += `\`\`\`${document.languageId}\n${selectedText}\n\`\`\`\n`;
  }

  // 光标周围的代码（上下 10 行）
  const cursorLine = selection.active.line;
  const startLine = Math.max(0, cursorLine - 10);
  const endLine = Math.min(document.lineCount - 1, cursorLine + 10);
  const surroundingText = document.getText(
    new vscode.Range(startLine, 0, endLine, 999),
  );
  context += `\nContext Around Cursor:\n\`\`\`${document.languageId}\n${surroundingText}\n\`\`\`\n`;

  return context;
}

// 在发送消息前自动注入
async function sendMessage(userMessage: string) {
  const context = await collectVSCodeContext();
  const fullMessage = context + '\n\nUser: ' + userMessage;
  await agent.sendPrompt(fullMessage);
}
```

### 5. **Checkpoint 系统** (Checkpointing)

**功能描述**：

```
自动保存代码状态：
- 每次 AI 修改前自动创建检查点
- 按 Esc 两次快速回退
- /rewind 命令回到之前版本
```

**当前状态**：

- ❌ 未实现

**实现难度**：⭐⭐⭐（中等）

**实现方案**：

```typescript
interface Checkpoint {
  id: string;
  timestamp: number;
  files: Map<string, string>; // filePath → content
  message: string;
}

class CheckpointManager {
  private checkpoints: Checkpoint[] = [];

  async createCheckpoint(message: string): Promise<string> {
    const checkpoint: Checkpoint = {
      id: Date.now().toString(),
      timestamp: Date.now(),
      files: new Map(),
      message,
    };

    // 保存所有打开的文件状态
    for (const editor of vscode.window.visibleTextEditors) {
      const uri = editor.document.uri;
      const content = editor.document.getText();
      checkpoint.files.set(uri.fsPath, content);
    }

    this.checkpoints.push(checkpoint);
    return checkpoint.id;
  }

  async rewind(steps: number = 1): Promise<void> {
    if (this.checkpoints.length < steps) {
      vscode.window.showWarningMessage('No more checkpoints to rewind');
      return;
    }

    const checkpoint = this.checkpoints[this.checkpoints.length - steps];

    // 恢复文件状态
    for (const [filePath, content] of checkpoint.files) {
      await vscode.workspace.fs.writeFile(
        vscode.Uri.file(filePath),
        Buffer.from(content),
      );
    }

    // 移除后续的检查点
    this.checkpoints = this.checkpoints.slice(0, -steps);
    vscode.window.showInformationMessage(`Rewound to: ${checkpoint.message}`);
  }
}

// 注册快捷键
vscode.commands.registerCommand('aionui.rewind', () => {
  checkpointManager.rewind(1);
});

// Esc 两次触发
let escPressCount = 0;
let escTimeout: NodeJS.Timeout;

vscode.commands.registerCommand('type', (args) => {
  if (args.text === '\u001b') {
    // Esc key
    escPressCount++;
    clearTimeout(escTimeout);

    if (escPressCount === 2) {
      checkpointManager.rewind(1);
      escPressCount = 0;
    } else {
      escTimeout = setTimeout(() => {
        escPressCount = 0;
      }, 500);
    }
  }
});
```

### 6. **Extended Thinking** (扩展思考)

**功能描述**：

```
显示 AI 的内部思考过程：
- 切换按钮控制是否显示
- 查看 AI 如何分析问题
- 理解 AI 的决策逻辑
```

**当前状态**：

- ⚠️ 部分实现（Qwen CLI 会输出 thoughts）

**实现难度**：⭐（简单）

**实现方案**：

```typescript
// 在 AcpConnection.ts 中
case 'session/update':
  const update = params.update;

  if (update.sessionUpdate === 'agent_message_chunk') {
    // 正常输出
    this.onStreamChunk(update.content?.text);
  }
  else if (update.sessionUpdate === 'thought') {
    // Extended Thinking 输出
    if (this.showThinking) {
      this.onThoughtChunk(`💭 ${update.content?.text}`);
    }
  }
  break;

// 添加切换按钮
<button onClick={() => setShowThinking(!showThinking)}>
  {showThinking ? '🧠 Hide Thinking' : '🧠 Show Thinking'}
</button>
```

### 7. **多会话支持** (Multiple Sessions)

**功能描述**：

```
在不同工作区文件夹中运行并行会话：
- 微服务架构支持
- 每个项目独立会话
- 快速切换上下文
```

**当前状态**：

- ✅ 已实现：跨项目会话查看
- ❌ 未实现：并行会话

**实现难度**：⭐⭐⭐⭐（较难）

**实现方案**：

```typescript
// 为每个 workspace folder 创建独立的 AgentManager
class MultiSessionManager {
  private sessions = new Map<string, QwenAgentManager>();

  async getOrCreateSession(workspaceFolder: string): Promise<QwenAgentManager> {
    if (!this.sessions.has(workspaceFolder)) {
      const agent = new QwenAgentManager();
      await agent.connect(workspaceFolder);
      this.sessions.set(workspaceFolder, agent);
    }
    return this.sessions.get(workspaceFolder)!;
  }

  async switchSession(workspaceFolder: string): Promise<void> {
    const agent = await this.getOrCreateSession(workspaceFolder);
    this.currentAgent = agent;
    // 更新 UI 显示
  }
}
```

### 8. **MCP 服务器支持** (MCP Support)

**功能描述**：

```
支持 Model Context Protocol 服务器：
- 通过 CLI 配置 MCP 服务器
- 扩展 AI 的工具能力
- 自定义工具集成
```

**当前状态**：

- ⚠️ 依赖 Qwen CLI 支持

**实现难度**：⭐⭐（简单，主要是配置）

**实现方案**：

```typescript
// Qwen CLI 已支持 MCP，我们只需要在 VSCode 设置中配置
{
  "aionui.qwen.mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/workspace"]
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_TOKEN": "${GITHUB_TOKEN}"
      }
    }
  }
}

// 在启动 Qwen CLI 时传递配置
const mcpConfig = vscode.workspace.getConfiguration('aionui.qwen').get('mcpServers');
await agent.connect(workingDir, mcpConfig);
```

---

## 功能对比矩阵

| 功能                 | Claude Code | AionUI 插件 | 实现难度 | 优先级    |
| -------------------- | ----------- | ----------- | -------- | --------- |
| **核心功能**         |
| 聊天界面             | ✅          | ✅          | -        | ✅ 已完成 |
| 流式响应             | ✅          | ✅          | -        | ✅ 已完成 |
| 会话历史             | ✅          | ✅          | -        | ✅ 已完成 |
| **上下文感知**       |
| 自动注入当前文件     | ✅          | ❌          | ⭐       | 🔥 P0     |
| 自动注入选中代码     | ✅          | ❌          | ⭐       | 🔥 P0     |
| 文件引用 (@filename) | ✅          | ❌          | ⭐⭐     | 🔥 P0     |
| 图片上传             | ✅          | ❌          | ⭐⭐     | P2        |
| **代码修改**         |
| 内联 Diff 预览       | ✅          | ❌          | ⭐⭐⭐   | 🔥 P0     |
| 一键接受/拒绝        | ✅          | ❌          | ⭐⭐     | 🔥 P0     |
| 多文件编辑           | ✅          | ⚠️          | ⭐⭐⭐   | P1        |
| **历史与撤销**       |
| Checkpoint 系统      | ✅          | ❌          | ⭐⭐⭐   | P1        |
| Esc 两次回退         | ✅          | ❌          | ⭐⭐     | P1        |
| /rewind 命令         | ✅          | ❌          | ⭐⭐     | P1        |
| **高级功能**         |
| Extended Thinking    | ✅          | ⚠️          | ⭐       | P1        |
| 多会话支持           | ✅          | ⚠️          | ⭐⭐⭐⭐ | P2        |
| MCP 服务器           | ✅          | ⚠️          | ⭐⭐     | P1        |
| **独特优势**         |
| 多 Agent 切换        | ❌          | ✅          | -        | ✅ 已完成 |
| 本地模型支持         | ❌          | ✅          | -        | ✅ 已完成 |
| 完全开源             | ❌          | ✅          | -        | ✅ 已完成 |

**优先级说明**：

- 🔥 **P0**：核心功能，立即实现（1-2 周）
- **P1**：重要功能，短期实现（1 个月）
- **P2**：增强功能，中期实现（2-3 个月）

---

## 实现优先级规划

### Phase 0：核心对标功能 (1-2 周) 🔥

目标：实现 Claude Code 的核心体验

#### 1. 自动上下文注入 ⭐

```typescript
// 文件：src/extension/services/ContextCollector.ts
export class ContextCollector {
  async collect(): Promise<string> {
    let context = '';

    // 当前文件
    const editor = vscode.window.activeTextEditor;
    if (editor) {
      context += `[Current File: ${editor.document.fileName}]\n`;
      context += editor.document.getText() + '\n\n';
    }

    // 选中代码
    if (editor && !editor.selection.isEmpty) {
      const selected = editor.document.getText(editor.selection);
      context += `[Selected Code]\n${selected}\n\n`;
    }

    return context;
  }
}
```

**工作量**：2 天
**优先级**：🔥🔥🔥🔥🔥

#### 2. 文件引用 (@filename) ⭐⭐

```typescript
// 文件：src/extension/services/FileReferenceParser.ts
export class FileReferenceParser {
  parse(message: string): {
    files: string[];
    cleanMessage: string;
  };

  async injectFileContents(
    message: string,
    workspaceRoot: string,
  ): Promise<string>;
}
```

**工作量**：3 天
**优先级**：🔥🔥🔥🔥🔥

#### 3. 内联 Diff 预览 ⭐⭐⭐

```typescript
// 文件：src/extension/services/DiffManager.ts
export class DiffManager {
  async showDiff(
    filePath: string,
    oldContent: string,
    newContent: string,
  ): Promise<'accept' | 'reject'>;

  async applyChanges(filePath: string, content: string): Promise<void>;
}
```

**工作量**：5 天
**优先级**：🔥🔥🔥🔥🔥

#### 4. 一键接受/拒绝修改 ⭐⭐

```typescript
// 在 Diff 视图中添加 QuickPick
const action = await vscode.window.showQuickPick([
  { label: '✅ Accept Changes', value: 'accept' },
  { label: '❌ Reject Changes', value: 'reject' },
  { label: '👁️  Review Later', value: 'later' },
]);
```

**工作量**：2 天
**优先级**：🔥🔥🔥🔥

### Phase 1：增强功能 (1 个月)

#### 5. Checkpoint 系统 ⭐⭐⭐

**工作量**：5 天
**优先级**：🔥🔥🔥

#### 6. Extended Thinking 切换 ⭐

**工作量**：2 天
**优先级**：🔥🔥🔥

#### 7. MCP 服务器配置 ⭐⭐

**工作量**：3 天
**优先级**：🔥🔥

### Phase 2：高级功能 (2-3 个月)

#### 8. 多会话并行 ⭐⭐⭐⭐

**工作量**：10 天
**优先级**：🔥🔥

#### 9. 图片上传支持 ⭐⭐

**工作量**：3 天
**优先级**：🔥

---

## 技术实现方案

### 方案 1：自动上下文注入

**架构设计**：

```
用户输入消息
    ↓
ContextCollector.collect()
    ├─ 获取当前文件
    ├─ 获取选中代码
    ├─ 获取打开的文件列表
    └─ 获取工作区信息
    ↓
构建完整的 prompt
    ↓
发送给 Agent
```

**代码实现**：

```typescript
// src/extension/services/ContextCollector.ts
export class ContextCollector {
  async collectFullContext(): Promise<{
    currentFile?: string;
    selectedCode?: string;
    openFiles: string[];
    workspaceInfo: string;
  }> {
    const context: any = {
      openFiles: [],
      workspaceInfo: '',
    };

    // 1. 当前文件
    const editor = vscode.window.activeTextEditor;
    if (editor) {
      context.currentFile = {
        path: editor.document.fileName,
        language: editor.document.languageId,
        content: editor.document.getText(),
      };

      // 2. 选中代码
      if (!editor.selection.isEmpty) {
        context.selectedCode = {
          text: editor.document.getText(editor.selection),
          startLine: editor.selection.start.line + 1,
          endLine: editor.selection.end.line + 1,
        };
      }
    }

    // 3. 打开的文件列表
    context.openFiles = vscode.window.visibleTextEditors
      .map((e) => e.document.fileName)
      .filter((v, i, a) => a.indexOf(v) === i); // 去重

    // 4. 工作区信息
    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (workspaceFolders) {
      context.workspaceInfo = workspaceFolders[0].uri.fsPath;
    }

    return context;
  }

  formatContext(context: any): string {
    let formatted = '[VSCode Context]\n\n';

    // 当前文件
    if (context.currentFile) {
      formatted += `## Current File: ${context.currentFile.path}\n`;
      formatted += `Language: ${context.currentFile.language}\n\n`;
      formatted += `\`\`\`${context.currentFile.language}\n`;
      formatted += context.currentFile.content;
      formatted += `\n\`\`\`\n\n`;
    }

    // 选中代码
    if (context.selectedCode) {
      formatted += `## Selected Code (lines ${context.selectedCode.startLine}-${context.selectedCode.endLine})\n`;
      formatted += `\`\`\`\n${context.selectedCode.text}\n\`\`\`\n\n`;
    }

    // 打开的文件
    if (context.openFiles.length > 0) {
      formatted += `## Open Files\n`;
      context.openFiles.forEach((file: string) => {
        formatted += `- ${file}\n`;
      });
      formatted += '\n';
    }

    return formatted;
  }
}
```

### 方案 2：文件引用解析

**解析策略**：

````
输入："请优化 @src/App.tsx 和 @src/utils/helper.ts"

步骤 1：正则匹配
  pattern: /@([\w\/\.\-]+)/g
  结果：['src/App.tsx', 'src/utils/helper.ts']

步骤 2：读取文件
  for each file:
    content = fs.readFile(file)

步骤 3：构建上下文
  [File: src/App.tsx]
  ```tsx
  ...file content...
````

[File: src/utils/helper.ts]

```ts
...file content...
```

User: 请优化 src/App.tsx 和 src/utils/helper.ts

````

**代码实现**：
```typescript
// src/extension/services/FileReferenceParser.ts
export class FileReferenceParser {
  private filePattern = /@([\w\/\.\-]+\.\w+)/g;

  parse(message: string): {
    files: string[];
    cleanMessage: string;
  } {
    const files: string[] = [];
    let match;

    while ((match = this.filePattern.exec(message)) !== null) {
      files.push(match[1]);
    }

    // 移除 @ 符号，保留文件名
    const cleanMessage = message.replace(this.filePattern, (_, file) => file);

    return { files, cleanMessage };
  }

  async injectFileContents(
    message: string,
    workspaceRoot: string
  ): Promise<string> {
    const { files, cleanMessage } = this.parse(message);

    if (files.length === 0) {
      return message;
    }

    let context = '';

    for (const file of files) {
      const fullPath = path.join(workspaceRoot, file);

      try {
        const content = await vscode.workspace.fs.readFile(
          vscode.Uri.file(fullPath)
        );
        const text = Buffer.from(content).toString('utf8');

        // 检测语言
        const ext = path.extname(file).slice(1);
        const lang = this.getLanguage(ext);

        context += `\n[File: ${file}]\n`;
        context += `\`\`\`${lang}\n${text}\n\`\`\`\n`;
      } catch (error) {
        context += `\n[File: ${file}] - Error: File not found\n`;
      }
    }

    return context + '\n\nUser: ' + cleanMessage;
  }

  private getLanguage(ext: string): string {
    const langMap: Record<string, string> = {
      ts: 'typescript',
      tsx: 'tsx',
      js: 'javascript',
      jsx: 'jsx',
      py: 'python',
      rs: 'rust',
      go: 'go',
      java: 'java',
      cpp: 'cpp',
      c: 'c',
      md: 'markdown',
      json: 'json',
      yaml: 'yaml',
      yml: 'yaml'
    };
    return langMap[ext] || ext;
  }
}
````

### 方案 3：内联 Diff 预览

**交互流程**：

```
1. Agent 请求写入文件
   ↓
2. 读取当前文件内容（oldContent）
   ↓
3. 打开 VSCode Diff 视图
   vscode.diff(oldUri, newUri, title)
   ↓
4. 显示 QuickPick 让用户选择
   ✅ Accept | ❌ Reject | 👁️ Review Later
   ↓
5. 根据选择执行操作
   - Accept: 写入新内容
   - Reject: 保持原样
   - Review: 保留 diff 视图，稍后决定
```

**代码实现**：

```typescript
// src/extension/services/DiffManager.ts
export class DiffManager {
  private pendingDiffs = new Map<
    string,
    {
      oldContent: string;
      newContent: string;
    }
  >();

  async showDiff(
    filePath: string,
    oldContent: string,
    newContent: string,
  ): Promise<'accept' | 'reject' | 'later'> {
    // 保存待处理的 diff
    this.pendingDiffs.set(filePath, { oldContent, newContent });

    // 创建虚拟文档 URI
    const oldUri = vscode.Uri.parse(`aionui-diff:${filePath}?version=old`).with(
      {
        query: Buffer.from(oldContent).toString('base64'),
      },
    );

    const newUri = vscode.Uri.parse(`aionui-diff:${filePath}?version=new`).with(
      {
        query: Buffer.from(newContent).toString('base64'),
      },
    );

    // 打开 diff 视图
    await vscode.commands.executeCommand(
      'vscode.diff',
      oldUri,
      newUri,
      `AI Changes: ${path.basename(filePath)}`,
    );

    // 显示操作选项
    const action = await vscode.window.showQuickPick(
      [
        {
          label: '$(check) Accept Changes',
          description: 'Apply AI modifications',
          value: 'accept',
        },
        {
          label: '$(x) Reject Changes',
          description: 'Keep original file',
          value: 'reject',
        },
        {
          label: '$(eye) Review Later',
          description: 'Keep diff open for review',
          value: 'later',
        },
      ],
      {
        placeHolder: 'Choose an action for AI changes',
      },
    );

    return (action?.value as any) || 'later';
  }

  async applyChanges(filePath: string): Promise<void> {
    const diff = this.pendingDiffs.get(filePath);
    if (!diff) return;

    await vscode.workspace.fs.writeFile(
      vscode.Uri.file(filePath),
      Buffer.from(diff.newContent),
    );

    this.pendingDiffs.delete(filePath);
    vscode.window.showInformationMessage(
      `✅ Applied changes to ${path.basename(filePath)}`,
    );
  }

  async rejectChanges(filePath: string): Promise<void> {
    this.pendingDiffs.delete(filePath);
    vscode.window.showInformationMessage(
      `❌ Rejected changes to ${path.basename(filePath)}`,
    );
  }
}

// 注册虚拟文档 provider
vscode.workspace.registerTextDocumentContentProvider('aionui-diff', {
  provideTextDocumentContent(uri: vscode.Uri): string {
    const content = uri.query;
    return Buffer.from(content, 'base64').toString('utf8');
  },
});
```

### 方案 4：集成到消息发送流程

**完整流程**：

```typescript
// src/extension/WebViewProvider.ts
private async handleSendMessage(text: string): Promise<void> {
  // 1. 收集 VSCode 上下文
  const contextCollector = new ContextCollector();
  const context = await contextCollector.collectFullContext();
  const contextStr = contextCollector.formatContext(context);

  // 2. 解析文件引用
  const fileParser = new FileReferenceParser();
  const workspaceRoot = vscode.workspace.workspaceFolders?.[0].uri.fsPath || '';
  const messageWithFiles = await fileParser.injectFileContents(text, workspaceRoot);

  // 3. 构建完整 prompt
  const fullPrompt = contextStr + '\n' + messageWithFiles;

  // 4. 创建检查点（如果启用）
  if (this.config.enableCheckpoints) {
    await this.checkpointManager.createCheckpoint(text);
  }

  // 5. 发送给 Agent
  await this.agentManager.sendMessage(fullPrompt);
}
```

---

## 实现时间表

### Week 1-2：P0 核心功能

- [x] Day 1-2: 自动上下文注入
- [x] Day 3-5: 文件引用解析
- [x] Day 6-10: 内联 Diff 预览
- [x] Day 11-12: 一键接受/拒绝

**里程碑**：实现 Claude Code 70% 的核心体验

### Week 3-4：P1 增强功能

- [x] Day 13-17: Checkpoint 系统
- [x] Day 18-19: Extended Thinking
- [x] Day 20-22: MCP 配置支持

**里程碑**：实现 Claude Code 90% 的功能

### Month 2-3：P2 高级功能

- [ ] Week 5-6: 多会话并行
- [ ] Week 7: 图片上传
- [ ] Week 8: UI/UX 优化

**里程碑**：功能完全对标 Claude Code

---

## 成功指标

### 功能完整度

- ✅ 核心聊天功能：100%
- ⏳ 上下文感知：0% → 目标 100%
- ⏳ 代码修改：0% → 目标 100%
- ⏳ 历史管理：0% → 目标 80%

### 用户体验

- ⏳ 自动化程度：提升 80%（减少手动操作）
- ⏳ 响应速度：< 100ms（上下文注入延迟）
- ⏳ 操作便捷性：接近 Claude Code

### 技术指标

- ⏳ 代码质量：保持 TypeScript 严格模式
- ⏳ 测试覆盖：核心功能 > 80%
- ⏳ 性能：内存占用 < 50MB

---

## 总结

### 与 Claude Code 的差异

**Claude Code 的优势**：

- ✅ 成熟的产品体验
- ✅ Anthropic 官方支持
- ✅ 与 Claude 模型深度集成

**我们的独特优势**：

- ✅ 多 Agent 支持（Claude/Qwen/Gemini）
- ✅ 完全开源
- ✅ 本地模型支持
- ✅ 企业私有部署

### 目标定位

**不是替代 Claude Code，而是提供更灵活的选择**：

- 对标 Claude Code 的功能
- 保持多 Agent 的灵活性
- 提供更好的隐私保护
- 打造开放的生态系统

---

## 下一步行动

1. ✅ **立即开始**：实现自动上下文注入（2 天）
2. ✅ **本周完成**：文件引用解析（3 天）
3. ✅ **两周内完成**：内联 Diff 预览（5 天）
4. ✅ **一个月内**：完成 P0 和 P1 所有功能

**让我们开始吧！🚀**
