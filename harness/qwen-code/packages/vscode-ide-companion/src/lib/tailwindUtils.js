// Tailwind CSS 工具类集合
// 用于封装常用的样式组合，便于在组件中复用

/**
 * 生成按钮样式类
 * @param {string} variant - 按钮变体: 'primary', 'secondary', 'ghost', 'icon'
 * @param {boolean} disabled - 是否禁用
 * @returns {string} Tailwind类字符串
 */
export const buttonClasses = (variant = 'primary', disabled = false) => {
  const baseClasses = 'inline-flex items-center justify-center rounded-md text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2';

  const variantClasses = {
    primary: 'bg-qwen-orange text-qwen-ivory hover:bg-qwen-clay-orange shadow-sm',
    secondary: 'bg-gray-100 text-gray-900 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-100 dark:hover:bg-gray-700',
    ghost: 'hover:bg-gray-100 dark:hover:bg-gray-800',
    icon: 'hover:bg-gray-100 dark:hover:bg-gray-800 p-1'
  };

  const disabledClasses = disabled ? 'opacity-50 pointer-events-none' : '';

  return `${baseClasses} ${variantClasses[variant] || variantClasses.primary} ${disabledClasses}`;
};

/**
 * 生成输入框样式类
 * @returns {string} Tailwind类字符串
 */
export const inputClasses = () => {
  return 'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background file:border-0 file:bg-transparent file:text-sm file:font-medium placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50';
};

/**
 * 生成卡片样式类
 * @returns {string} Tailwind类字符串
 */
export const cardClasses = () => {
  return 'rounded-lg border bg-card text-card-foreground shadow-sm';
};

/**
 * 生成对话框样式类
 * @returns {string} Tailwind类字符串
 */
export const dialogClasses = () => {
  return 'fixed inset-0 z-50 flex items-center justify-center bg-black bg-opacity-50';
};

/**
 * 生成Qwen品牌颜色类
 * @param {string} color - 颜色名称: 'orange', 'clay-orange', 'ivory', 'slate', 'green'
 * @returns {string} Tailwind类字符串
 */
export const qwenColorClasses = (color) => {
  const colorMap = {
    'orange': 'text-qwen-orange',
    'clay-orange': 'text-qwen-clay-orange',
    'ivory': 'text-qwen-ivory',
    'slate': 'text-qwen-slate',
    'green': 'text-qwen-green'
  };

  return colorMap[color] || 'text-qwen-orange';
};

/**
 * 生成间距类
 * @param {string} size - 尺寸: 'small', 'medium', 'large', 'xlarge'
 * @param {string} direction - 方向: 'all', 'x', 'y', 't', 'r', 'b', 'l'
 * @returns {string} Tailwind类字符串
 */
export const spacingClasses = (size = 'medium', direction = 'all') => {
  const sizeMap = {
    'small': 'small',
    'medium': 'medium',
    'large': 'large',
    'xlarge': 'xlarge'
  };

  const directionMap = {
    'all': 'p',
    'x': 'px',
    'y': 'py',
    't': 'pt',
    'r': 'pr',
    'b': 'pb',
    'l': 'pl'
  };

  return `${directionMap[direction]}-${sizeMap[size]}`;
};

/**
 * 生成圆角类
 * @param {string} size - 尺寸: 'small', 'medium', 'large'
 * @returns {string} Tailwind类字符串
 */
export const borderRadiusClasses = (size = 'medium') => {
  const sizeMap = {
    'small': 'rounded-small',
    'medium': 'rounded-medium',
    'large': 'rounded-large'
  };

  return sizeMap[size] || 'rounded-medium';
};

// 导出常用的类组合
export const commonClasses = {
  // 布局类
  flexCenter: 'flex items-center justify-center',
  flexBetween: 'flex items-center justify-between',
  flexCol: 'flex flex-col',

  // 文本类
  textMuted: 'text-gray-500 dark:text-gray-400',
  textSmall: 'text-sm',
  textLarge: 'text-lg',
  fontWeightMedium: 'font-medium',
  fontWeightSemibold: 'font-semibold',

  // 间距类
  marginAuto: 'm-auto',
  fullWidth: 'w-full',
  fullHeight: 'h-full',

  // 其他常用类
  truncate: 'truncate',
  srOnly: 'sr-only',
  transition: 'transition-all duration-200 ease-in-out'
};