# Tailwind CSS 渐进式集成计划

## 1. 当前状态分析

### 已完成的配置
- [x] Tailwind CSS 已添加到项目依赖
- [x] `tailwind.config.js` 配置文件已创建
- [x] `postcss.config.js` 配置文件已创建
- [x] `src/styles.css` 已创建并包含Tailwind指令

### 现有样式系统
- SCSS/Sass 预处理器
- CSS Modules 组件样式
- 传统CSS文件
- 从Claude Code提取的样式

## 2. 渐进式集成策略

### 阶段1：基础配置和工具类实现
- 创建可复用的Tailwind工具类集合
- 建立设计系统变量映射
- 配置Tailwind主题以匹配现有设计

### 阶段2：组件样式重构
- 选择性地重构部分组件以使用Tailwind
- 保持向后兼容性
- 逐步替换现有CSS类

### 阶段3：全面集成
- 将Tailwind应用到所有组件
- 移除冗余的CSS代码
- 优化构建配置

## 3. 设计系统映射

### 颜色映射
| 现有变量 | Tailwind 类 | 说明 |
|---------|------------|------|
| `--app-qwen-orange` | `text-indigo-500`, `bg-indigo-500` | 主要品牌色 |
| `--app-primary-foreground` | `text-gray-900`, `text-white` | 主要前景色 |
| `--app-secondary-foreground` | `text-gray-500`, `text-gray-400` | 次要前景色 |
| `--app-primary-background` | `bg-gray-50`, `bg-gray-900` | 主要背景色 |
| `--app-input-background` | `bg-white`, `bg-gray-800` | 输入框背景色 |

### 间距映射
| 现有变量 | Tailwind 类 | 说明 |
|---------|------------|------|
| `--app-spacing-small` (4px) | `p-1`, `m-1` | 小间距 |
| `--app-spacing-medium` (8px) | `p-2`, `m-2` | 中等间距 |
| `--app-spacing-large` (12px) | `p-3`, `m-3` | 大间距 |
| `--app-spacing-xlarge` (16px) | `p-4`, `m-4` | 超大间距 |

### 圆角映射
| 现有变量 | Tailwind 类 | 说明 |
|---------|------------|------|
| `--corner-radius-small` (4px) | `rounded` | 小圆角 |
| `--corner-radius-medium` (6px) | `rounded-md` | 中等圆角 |
| `--corner-radius-large` (8px) | `rounded-lg` | 大圆角 |

## 4. 实施步骤

### 第一步：更新Tailwind配置
```js
// tailwind.config.js
module.exports = {
  content: [
    "./src/**/*.{js,jsx,ts,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        'qwen': {
          'orange': '#615fff',
          'clay-orange': '#4f46e5',
          'ivory': '#f5f5ff',
          'slate': '#141420',
          'green': '#6bcf7f',
        }
      },
      borderRadius: {
        'small': '4px',
        'medium': '6px',
        'large': '8px',
      }
    },
  },
  plugins: [],
}
```

### 第二步：创建可复用组件类
创建 `src/lib/tailwindUtils.js` 文件，导出常用的Tailwind类组合。

### 第三步：逐步重构组件
选择几个关键组件开始重构，例如：
1. `PermissionDrawer` 组件
2. `SaveSessionDialog` 组件
3. 输入框组件

## 5. 注意事项

1. **向后兼容性**：确保现有样式在重构过程中不受影响
2. **性能优化**：配置Tailwind的内容扫描路径以避免不必要的CSS生成
3. **团队协作**：建立Tailwind使用规范和最佳实践
4. **测试验证**：在不同主题（深色/浅色）下验证样式效果