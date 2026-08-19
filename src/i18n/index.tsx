import { createContext, useCallback, useContext, useEffect, useState } from "react";

/// 多语言。
///
/// **key 就是中文原文**，不另发明一套 `page.title` 之类的 id。理由：
///   * 迁移成本近乎为零 —— 把 `总览` 改成 `t("总览")` 就完成了一处。
///   * 天然降级 —— 英文词典里没有的条目直接回落成中文，不会出现空白或
///     `missing.key` 这种东西，可以一页一页地翻而不必一次翻完。
///   * 读代码时就是读界面 —— 不用跳到词典去猜这个 id 长什么样。
/// 代价是改中文文案会断开翻译链接；这跟 gettext 的取舍一样，可以接受。

export type Lang = "zh-CN" | "en";

const STORAGE_KEY = "ccload.lang";

/** 未选过时跟随系统：只要不是中文环境就给英文。 */
function detect(): Lang {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === "zh-CN" || saved === "en") return saved;
  return navigator.language?.toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

type Dict = Record<string, string>;

const DICTS: Record<Lang, Dict> = {
  "zh-CN": {}, // 中文即 key 本身
  en: {},
};

/** 英文词典由 en.ts 注册进来，避免 i18n 核心反过来依赖具体语言文件。 */
export function registerDict(lang: Lang, dict: Dict) {
  Object.assign(DICTS[lang], dict);
}

export type Translate = (zh: string, vars?: Record<string, string | number>) => string;

const Ctx = createContext<{ lang: Lang; setLang: (l: Lang) => void; t: Translate }>({
  lang: "zh-CN",
  setLang: () => {},
  t: (s) => s,
});

export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detect);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  const setLang = useCallback((l: Lang) => {
    localStorage.setItem(STORAGE_KEY, l);
    setLangState(l);
  }, []);

  const t = useCallback<Translate>(
    (zh, vars) => {
      const raw = DICTS[lang][zh] ?? zh;
      if (!vars) return raw;
      // `{n}` 占位。故意不做复数/性数变化：这个界面里没有需要它的句子，
      // 引入一套复数规则只会让每条文案都更难写。
      return raw.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
    },
    [lang],
  );

  return <Ctx.Provider value={{ lang, setLang, t }}>{children}</Ctx.Provider>;
}

export function useI18n() {
  return useContext(Ctx);
}

/** 只要翻译函数时用这个，少解构一层。 */
export function useT(): Translate {
  return useContext(Ctx).t;
}
