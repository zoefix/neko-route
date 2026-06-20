import React from "react";
import { CATALOGS, type Lang, type MsgKey, type Vars, translate } from "./messages";

type I18n = {
  lang: Lang;
  setLang: (lang: Lang) => void;
  t: (key: MsgKey, vars?: Vars) => string;
};

const I18nContext = React.createContext<I18n | null>(null);

const STORAGE_KEY = "neko-route.lang";

function detectLang(): Lang {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored && stored in CATALOGS) return stored as Lang;
  const nav = navigator.language.toLowerCase();
  if (nav.startsWith("ja")) return "ja";
  if (nav.startsWith("zh")) {
    return nav.includes("tw") || nav.includes("hk") || nav.includes("hant")
      ? "zh-TW"
      : "zh-CN";
  }
  if (nav.startsWith("en")) return "en";
  return "zh-CN";
}

export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [lang, setLangState] = React.useState<Lang>(() => detectLang());

  const setLang = React.useCallback((next: Lang) => {
    setLangState(next);
    localStorage.setItem(STORAGE_KEY, next);
    document.documentElement.lang = next;
  }, []);

  React.useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  const t = React.useCallback(
    (key: MsgKey, vars?: Vars) => translate(lang, key, vars),
    [lang],
  );

  const value = React.useMemo<I18n>(() => ({ lang, setLang, t }), [lang, setLang, t]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18n {
  const ctx = React.useContext(I18nContext);
  if (!ctx) throw new Error("useI18n must be used within I18nProvider");
  return ctx;
}
