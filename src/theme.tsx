import React from "react";

export type ThemeChoice = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

type ThemeCtx = {
  choice: ThemeChoice;
  resolved: ResolvedTheme;
  setChoice: (c: ThemeChoice) => void;
};

const Ctx = React.createContext<ThemeCtx | null>(null);
const STORAGE_KEY = "neko-route.theme";

function systemTheme(): ResolvedTheme {
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function detectChoice(): ThemeChoice {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === "light" || stored === "dark" || stored === "system") return stored;
  return "system";
}

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [choice, setChoiceState] = React.useState<ThemeChoice>(() => detectChoice());
  const [resolved, setResolved] = React.useState<ResolvedTheme>(() =>
    detectChoice() === "system" ? systemTheme() : (detectChoice() as ResolvedTheme),
  );

  const apply = React.useCallback((c: ThemeChoice) => {
    const r = c === "system" ? systemTheme() : c;
    setResolved(r);
    document.documentElement.setAttribute("data-theme", r);
  }, []);

  const setChoice = React.useCallback(
    (c: ThemeChoice) => {
      setChoiceState(c);
      localStorage.setItem(STORAGE_KEY, c);
      apply(c);
    },
    [apply],
  );

  React.useEffect(() => {
    apply(choice);
  }, [choice, apply]);

  // Follow OS changes while in "system" mode.
  React.useEffect(() => {
    if (choice !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => apply("system");
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [choice, apply]);

  const value = React.useMemo<ThemeCtx>(
    () => ({ choice, resolved, setChoice }),
    [choice, resolved, setChoice],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useTheme(): ThemeCtx {
  const ctx = React.useContext(Ctx);
  if (!ctx) throw new Error("useTheme must be used within ThemeProvider");
  return ctx;
}
