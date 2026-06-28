import React from "react";
import {
  IconCheck as CheckIcon,
  IconChevronDown as ChevronDown,
  IconDeviceDesktop as Monitor,
  IconLoader2 as Loader2,
  IconMoon as Moon,
  IconSun as Sun,
  IconX as X,
} from "@tabler/icons-react";
import { createPortal } from "react-dom";
import { useI18n } from "./i18n";
import { useTheme, type ThemeChoice } from "./theme";
import { LANGS, type Lang } from "./messages";

/* ---------- Button ---------- */
type BtnProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "ghost" | "mint" | "danger";
  icon?: React.ReactNode;
  loading?: boolean;
};
export function Button({
  variant = "ghost",
  icon,
  loading,
  children,
  className = "",
  disabled,
  ...rest
}: BtnProps) {
  return (
    <button
      className={`btn btn-${variant} ${className}`}
      disabled={disabled || loading}
      {...rest}
    >
      {loading ? <Loader2 size={17} className="spin" /> : icon}
      {children}
    </button>
  );
}

export function IconButton({
  icon,
  danger,
  className = "",
  ...rest
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  icon: React.ReactNode;
  danger?: boolean;
}) {
  return (
    <button className={`icon-btn ${danger ? "danger" : ""} ${className}`} {...rest}>
      {icon}
    </button>
  );
}

/* ---------- Switch (standalone toggle) ---------- */
export function Switch({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: string;
}) {
  return (
    <label className="toggle">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="track" />
      {label ? <span>{label}</span> : null}
    </label>
  );
}

/* ---------- Seed-on-open hook ----------
   Runs `seed` only when `open` transitions false -> true, so a parent
   re-render (e.g. background refresh) never resets in-progress form input. */
export function useSeedOnOpen(open: boolean, seed: () => void) {
  const prev = React.useRef(false);
  const seedRef = React.useRef(seed);
  seedRef.current = seed;
  React.useEffect(() => {
    if (open && !prev.current) seedRef.current();
    prev.current = open;
  }, [open]);
}

/* ---------- Field + inputs ---------- */
export function Field({
  label,
  hint,
  children,
  className = "",
}: {
  label?: string;
  hint?: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={`field ${className}`}>
      {label ? <span className="label">{label}</span> : null}
      {children}
      {hint ? <span className="field-hint">{hint}</span> : null}
    </div>
  );
}

export const Input = React.forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement>>(
  (props, ref) => <input ref={ref} className="input" {...props} />,
);
Input.displayName = "Input";

/* ---------- Status pill ---------- */
export function Pill({
  tone,
  label,
}: {
  tone: "ok" | "warn" | "bad";
  label: string;
}) {
  return (
    <span className={`pill ${tone}`}>
      <span className="dot" />
      {label}
    </span>
  );
}

/* ---------- Panel ---------- */
type IconColor = "sakura" | "mint" | "lav" | "sky" | "peach";
export function Panel({
  title,
  sub,
  icon,
  color = "lav",
  right,
  children,
  className = "",
}: {
  title: string;
  sub?: string;
  icon: React.ReactNode;
  color?: IconColor;
  right?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={`card card-pad ${className}`}>
      <div className="panel-head">
        <span className={`head-icon tone-${color}`}>{icon}</span>
        <div className="head-titles">
          <h2>{title}</h2>
          {sub ? <p className="head-sub">{sub}</p> : null}
        </div>
        {right ? <div className="head-right">{right}</div> : null}
      </div>
      {children}
    </section>
  );
}

/* ---------- Metric row ---------- */
export function Metric({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="metric">
      <span className="m-label">{label}</span>
      <span className="m-value">{value}</span>
    </div>
  );
}

/* ---------- Stat tile ---------- */
export function Stat({
  icon,
  label,
  value,
  foot,
  grad,
}: {
  icon: React.ReactNode;
  label: string;
  value: React.ReactNode;
  foot?: string;
  grad?: boolean;
}) {
  return (
    <div className="stat">
      <div className="stat-top">
        {icon}
        {label}
      </div>
      <div className={`stat-value ${grad ? "grad" : ""}`}>{value}</div>
      {foot ? <div className="stat-foot">{foot}</div> : null}
    </div>
  );
}

/* ---------- Empty state ---------- */
export function Empty({ icon, title, hint }: { icon: React.ReactNode; title: string; hint?: string }) {
  return (
    <div className="empty">
      <span className="e-icon">{icon}</span>
      <span className="e-title">{title}</span>
      {hint ? <span className="e-hint">{hint}</span> : null}
    </div>
  );
}

/* ---------- Custom Dropdown ---------- */
export type Option = { value: string; label: string; sub?: string; tone?: "ok" | "warn" | "bad" };
export function Dropdown({
  value,
  options,
  onChange,
  placeholder,
}: {
  value: string;
  options: Option[];
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  const [open, setOpen] = React.useState(false);
  const ref = React.useRef<HTMLDivElement>(null);
  const selected = options.find((o) => o.value === value);
  const selectedToneClass = selected?.tone ? `dd-tone-${selected.tone}` : "";

  React.useEffect(() => {
    if (!open) return;
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className={`dropdown ${open ? "open" : ""}`} ref={ref}>
      <button type="button" className={`dropdown-trigger ${selectedToneClass}`} onClick={() => setOpen((o) => !o)}>
        <span className={selected ? "" : "dd-placeholder"}>
          {selected ? selected.label : placeholder ?? ""}
        </span>
        <ChevronDown size={16} className="dd-chevron" />
      </button>
      {open ? (
        <div className="dropdown-menu" role="listbox">
          {options.map((o) => (
            <button
              type="button"
              key={o.value}
              role="option"
              aria-selected={o.value === value}
              className={`dropdown-item ${o.value === value ? "active" : ""} ${o.tone ? `dd-tone-${o.tone}` : ""}`}
              onClick={() => {
                onChange(o.value);
                setOpen(false);
              }}
            >
              <span className="dd-label">{o.label}</span>
              {o.sub ? <span className="dd-sub">{o.sub}</span> : null}
              {o.value === value ? <CheckIcon size={15} className="dd-check" /> : null}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/* ---------- Combobox: editable input + fetched suggestions ----------
   The user can type a free value, or pick a suggestion. Picking calls
   onPick (separate from onChange) so callers can react to a deliberate
   selection (e.g. auto-fill another field). */
export function Combobox({
  value,
  options,
  loading,
  onChange,
  onPick,
  placeholder,
  emptyHint,
}: {
  value: string;
  options: Option[];
  loading?: boolean;
  onChange: (v: string) => void;
  onPick: (option: Option) => void;
  placeholder?: string;
  emptyHint?: string;
}) {
  const [open, setOpen] = React.useState(false);
  const [typing, setTyping] = React.useState(false);
  const ref = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    if (!open) return;
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // Filter only while the user is actively typing a query; opening via the
  // toggle or focus shows the full list (so a stale value never hides options).
  const q = value.trim().toLowerCase();
  const filtered = typing && q
    ? options.filter((o) => o.value.toLowerCase().includes(q) || o.label.toLowerCase().includes(q))
    : options;

  return (
    <div className={`dropdown combobox ${open ? "open" : ""}`} ref={ref}>
      <div className="combobox-field">
        <input
          className="input"
          value={value}
          placeholder={placeholder}
          onChange={(e) => {
            setTyping(true);
            setOpen(true);
            onChange(e.target.value);
          }}
          onFocus={() => {
            setTyping(false);
            setOpen(true);
          }}
          onClick={() => setOpen(true)}
        />
        <button
          type="button"
          className="combobox-toggle"
          tabIndex={-1}
          onClick={() => {
            setTyping(false);
            setOpen((o) => !o);
          }}
          aria-label="toggle"
        >
          {loading ? <Loader2 size={15} className="spin" /> : <ChevronDown size={15} className="dd-chevron" />}
        </button>
      </div>
      {open ? (
        <div className="dropdown-menu" role="listbox">
          {loading ? (
            <div className="combo-status"><Loader2 size={14} className="spin" /> …</div>
          ) : filtered.length === 0 ? (
            <div className="combo-status">{emptyHint ?? "—"}</div>
          ) : (
            filtered.map((o) => (
              <button
                type="button"
                key={o.value}
                role="option"
                aria-selected={o.value === value}
                className={`dropdown-item ${o.value === value ? "active" : ""}`}
                onClick={() => {
                  onPick(o);
                  setOpen(false);
                }}
              >
                <span className="dd-label">{o.label}</span>
                {o.sub || o.label !== o.value ? <span className="dd-sub">{o.sub ?? o.value}</span> : null}
                {o.value === value ? <CheckIcon size={15} className="dd-check" /> : null}
              </button>
            ))
          )}
        </div>
      ) : null}
    </div>
  );
}

/* ---------- Modal ---------- */
export function Modal({
  open,
  onClose,
  title,
  sub,
  icon,
  color = "lav",
  children,
  footer,
  width = 540,
  onEnter,
  showClose = true,
  closeOnOverlay = true,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  sub?: string;
  icon?: React.ReactNode;
  color?: IconColor;
  children: React.ReactNode;
  footer?: React.ReactNode;
  width?: number;
  onEnter?: () => void;
  showClose?: boolean;
  closeOnOverlay?: boolean;
}) {
  const [mounted, setMounted] = React.useState(open);
  const [visible, setVisible] = React.useState(false);

  React.useEffect(() => {
    if (open) {
      setMounted(true);
      const id = requestAnimationFrame(() => setVisible(true));
      return () => cancelAnimationFrame(id);
    }
    setVisible(false);
    const t = window.setTimeout(() => setMounted(false), 240);
    return () => window.clearTimeout(t);
  }, [open]);

  React.useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
      if (
        e.key === "Enter" &&
        onEnter &&
        !e.shiftKey &&
        !(e.target instanceof HTMLTextAreaElement)
      ) {
        e.preventDefault();
        onEnter();
      }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose, onEnter]);

  if (!mounted) return null;

  return createPortal(
    <div
      className={`modal-overlay ${visible ? "show" : ""}`}
      onMouseDown={
        closeOnOverlay
          ? (e) => {
              // 仅当直接点击遮罩层(而非模态内容)时关闭；不再 stopPropagation，
              // 这样模态内的 mousedown 能冒泡到 document，下拉框的点击外部关闭才生效。
              if (e.target === e.currentTarget) onClose();
            }
          : undefined
      }
    >
      <div
        className={`modal ${visible ? "show" : ""}`}
        style={{ maxWidth: width }}
        role="dialog"
        aria-modal="true"
      >
        <div className="modal-head">
          {icon ? <span className={`head-icon tone-${color}`}>{icon}</span> : null}
          <div className="head-titles">
            <h2>{title}</h2>
            {sub ? <p className="head-sub">{sub}</p> : null}
          </div>
          {showClose ? (
            <button className="icon-btn modal-x" onClick={onClose} aria-label="close">
              <X size={18} />
            </button>
          ) : null}
        </div>
        <div className="modal-body">{children}</div>
        {footer ? <div className="modal-foot">{footer}</div> : null}
      </div>
    </div>,
    document.body,
  );
}

/* ---------- Confirm dialog ---------- */
export function ConfirmDialog({
  open,
  onClose,
  onConfirm,
  title,
  body,
  confirmLabel,
  icon,
  tone = "danger",
  loading,
}: {
  open: boolean;
  onClose: () => void;
  onConfirm: () => void;
  title: string;
  body: React.ReactNode;
  confirmLabel: string;
  icon?: React.ReactNode;
  tone?: "danger" | "primary";
  loading?: boolean;
}) {
  const { t } = useI18n();
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={title}
      icon={icon}
      color={tone === "danger" ? "sakura" : "lav"}
      width={420}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button variant={tone === "danger" ? "danger" : "primary"} onClick={onConfirm} loading={loading}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <p className="confirm-body">{body}</p>
    </Modal>
  );
}

/* ---------- Language switcher ---------- */
export function LangSwitch() {
  const { lang, setLang, t } = useI18n();
  const [open, setOpen] = React.useState(false);
  const ref = React.useRef<HTMLDivElement>(null);
  const current = LANGS.find((l) => l.code === lang)!;

  React.useEffect(() => {
    if (!open) return;
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  return (
    <div className={`langswitch ${open ? "open" : ""}`} ref={ref}>
      <button className="icon-btn lang-trigger" onClick={() => setOpen((o) => !o)} title={t("topbar.language")}>
        <span className="lang-short">{current.short}</span>
      </button>
      {open ? (
        <div className="lang-menu">
          {LANGS.map((l) => (
            <button
              key={l.code}
              className={`lang-item ${l.code === lang ? "active" : ""}`}
              onClick={() => {
                setLang(l.code as Lang);
                setOpen(false);
              }}
            >
              <span className="lang-badge">{l.short}</span>
              {l.label}
              {l.code === lang ? <CheckIcon size={15} className="dd-check" /> : null}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/* ---------- Theme switcher (light / dark / system) ---------- */
export function ThemeSwitch() {
  const { choice, resolved, setChoice } = useTheme();
  const { t } = useI18n();
  const [open, setOpen] = React.useState(false);
  const ref = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    if (!open) return;
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const options: { value: ThemeChoice; label: string; icon: React.ReactNode }[] = [
    { value: "light", label: t("theme.light"), icon: <Sun size={16} /> },
    { value: "dark", label: t("theme.dark"), icon: <Moon size={16} /> },
    { value: "system", label: t("theme.system"), icon: <Monitor size={16} /> },
  ];
  const triggerIcon = resolved === "dark" ? <Moon size={18} /> : <Sun size={18} />;

  return (
    <div className={`langswitch ${open ? "open" : ""}`} ref={ref}>
      <button className="icon-btn" onClick={() => setOpen((o) => !o)} title={t("theme.label")}>
        {triggerIcon}
      </button>
      {open ? (
        <div className="lang-menu">
          {options.map((o) => (
            <button
              key={o.value}
              className={`lang-item ${o.value === choice ? "active" : ""}`}
              onClick={() => {
                setChoice(o.value);
                setOpen(false);
              }}
            >
              <span className="lang-badge">{o.icon}</span>
              {o.label}
              {o.value === choice ? <CheckIcon size={15} className="dd-check" /> : null}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
