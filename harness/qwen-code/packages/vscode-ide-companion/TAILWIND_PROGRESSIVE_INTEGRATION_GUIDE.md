# 渐进式Tailwind CSS集成指南

## 1. 集成策略

我们采用渐进式的方式将Tailwind CSS集成到现有项目中，确保不影响现有功能：

1. **保持现有样式**：不修改现有的CSS文件和类名
2. **并行使用**：在新组件或组件重构时使用Tailwind
3. **逐步替换**：随着时间推移，逐步将现有CSS替换为Tailwind类

## 2. 实施步骤

### 步骤1：创建混合样式组件

我们可以创建同时使用传统CSS和Tailwind的组件：

```tsx
// 示例：在现有组件中添加Tailwind类
import './PermissionDrawer.css'; // 保留现有样式

export const PermissionDrawer: React.FC<PermissionDrawerProps> = ({ ... }) => {
  return (
    <div className="permission-drawer rounded-lg shadow-lg"> {/* 混合使用 */}
      {/* 现有内容 */}
    </div>
  );
};
```

### 步骤2：创建Tailwind增强版本

为现有组件创建Tailwind增强版本，供新功能使用：

```tsx
// PermissionDrawer.tailwind.tsx
export const PermissionDrawerTailwind: React.FC<PermissionDrawerProps> = ({ ... }) => {
  return (
    <div className="fixed inset-0 flex items-end justify-center">
      <div className="bg-white rounded-t-xl shadow-2xl w-full max-w-md">
        {/* 使用纯Tailwind样式 */}
      </div>
    </div>
  );
};
```

### 步骤3：创建可复用的Tailwind组件

创建新的小组件，专门使用Tailwind样式：

```tsx
// components/ui/Button.tsx
interface ButtonProps {
  variant?: 'primary' | 'secondary' | 'ghost';
  size?: 'sm' | 'md' | 'lg';
  children: React.ReactNode;
}

export const Button: React.FC<ButtonProps> = ({
  variant = 'primary',
  size = 'md',
  children
}) => {
  const baseClasses = "inline-flex items-center justify-center rounded-md font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";

  const variantClasses = {
    primary: "bg-qwen-orange text-white hover:bg-qwen-clay-orange",
    secondary: "bg-gray-100 text-gray-900 hover:bg-gray-200",
    ghost: "hover:bg-gray-100"
  };

  const sizeClasses = {
    sm: "h-8 px-3 text-xs",
    md: "h-10 px-4 py-2 text-sm",
    lg: "h-12 px-6 text-base"
  };

  return (
    <button
      className={`${baseClasses} ${variantClasses[variant]} ${sizeClasses[size]}`}
    >
      {children}
    </button>
  );
};
```

## 3. 最佳实践

### 保持向后兼容性
- 不要删除现有的CSS文件
- 逐步替换，而不是一次性重构
- 在团队中建立清晰的规范

### 性能优化
```js
// tailwind.config.js - 优化内容扫描
module.exports = {
  content: [
    "./src/**/*.{js,jsx,ts,tsx}", // 只扫描实际使用的文件
  ],
  // ...
};
```

### 设计系统一致性
```js
// tailwind.config.js - 扩展主题以匹配现有设计
module.exports = {
  theme: {
    extend: {
      colors: {
        'qwen': {
          'orange': '#615fff',
          // ... 其他品牌色
        }
      },
      borderRadius: {
        'small': '4px',
        'medium': '6px',
        'large': '8px',
      }
    },
  },
};
```

## 4. 示例：逐步重构PermissionDrawer

### 当前状态（保留现有样式）
```tsx
// PermissionDrawer.tsx
import './PermissionDrawer.css'; // 保留现有样式

export const PermissionDrawer = ({ ... }) => {
  return (
    <div className="permission-drawer">
      {/* 使用现有CSS类 */}
    </div>
  );
};
```

### 增强版本（添加Tailwind辅助类）
```tsx
// PermissionDrawer.tsx
import './PermissionDrawer.css'; // 保留现有样式

export const PermissionDrawer = ({ ... }) => {
  return (
    <div className="permission-drawer flex flex-col"> {/* 添加Tailwind类作为补充 */}
      {/* 使用现有CSS类 */}
    </div>
  );
};
```

### 完全Tailwind版本（可选的未来状态）
```tsx
// PermissionDrawer.tailwind.tsx
export const PermissionDrawerTailwind = ({ ... }) => {
  return (
    <div className="fixed inset-0 flex items-end justify-center">
      <div className="bg-white rounded-t-xl shadow-2xl w-full max-w-md flex flex-col">
        {/* 完全使用Tailwind类 */}
      </div>
    </div>
  );
};
```

## 5. 团队协作建议

1. **文档化**：维护一个Tailwind使用指南
2. **代码审查**：在代码审查中检查Tailwind使用是否合理
3. **组件库**：逐步建立基于Tailwind的组件库
4. **培训**：为团队成员提供Tailwind培训