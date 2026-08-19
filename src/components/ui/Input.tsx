import { forwardRef } from "react";
import { ChevronDown } from "lucide-react";
import { cn } from "../../lib/cn";

/// 全部文本控件的唯一出口。
///
/// 两个作用，缺一不可：
///  1. 外观统一 —— 圆角/内边距/边框/hover 全在 `.field`（见 index.css），调用点
///     只写尺寸修饰符，不再各拼一串 class。
///  2. 默认关掉自动大写、自动更正、拼写检查。这里写的几乎全是模型 ID、环境变量
///     名、shell 命令、URL，被首字母大写成 `Claude-opus-5` 就是一条写不进去的
///     配置。HTML 这三个属性只覆盖 WebKit 这一层，系统级的自动大写另外在
///     src-tauri/src/platform/macos.rs 里关。
///
/// 真需要自动大写的场景（目前没有）显式传 `autoCapitalize="sentences"` 覆盖。

const NO_AUTO = {
  autoCapitalize: "off",
  autoCorrect: "off",
  spellCheck: false,
} as const;

type InputProps = React.InputHTMLAttributes<HTMLInputElement> & {
  /** 等宽字体：模型 ID、KEY、命令、URL 这类要逐字符看清的内容。 */
  mono?: boolean;
  /** 紧凑尺寸：表格内、筛选栏。 */
  small?: boolean;
};

export const TextInput = forwardRef<HTMLInputElement, InputProps>(function TextInput(
  { mono, small, className, ...rest },
  ref,
) {
  return (
    <input
      ref={ref}
      {...NO_AUTO}
      {...rest}
      className={cn("field", mono && "field-mono", small && "field-sm", className)}
    />
  );
});

type TextAreaProps = React.TextareaHTMLAttributes<HTMLTextAreaElement> & {
  mono?: boolean;
};

export const TextArea = forwardRef<HTMLTextAreaElement, TextAreaProps>(function TextArea(
  { mono, className, ...rest },
  ref,
) {
  return (
    <textarea
      ref={ref}
      {...NO_AUTO}
      {...rest}
      className={cn("field", mono && "field-mono", className)}
    />
  );
});

type SelectProps = React.SelectHTMLAttributes<HTMLSelectElement> & {
  small?: boolean;
};

/// 原生 select 的箭头关掉了（见 index.css），这里补一个和侧栏同一套的 chevron。
/// 包一层 relative 的 span 而不是 div：select 常常出现在 flex 行里，span 不会
/// 意外改变行内布局；宽度由外部 className 决定，wrapper 跟着长。
export const Select = forwardRef<HTMLSelectElement, SelectProps>(function Select(
  { small, className, children, ...rest },
  ref,
) {
  return (
    <span className={cn("relative inline-flex items-center", className)}>
      <select
        ref={ref}
        {...rest}
        className={cn("field w-full", small && "field-sm")}
      >
        {children}
      </select>
      <ChevronDown
        aria-hidden
        className={cn(
          "pointer-events-none absolute right-2 text-muted",
          small ? "h-3 w-3" : "h-3.5 w-3.5",
        )}
      />
    </span>
  );
});
