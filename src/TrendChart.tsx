import React from "react";
import type { DayTokens, ModelDaySeries } from "./types";
import { formatTokens } from "./providers";

type TrendLabels = {
  total: string;
  input: string;
  output: string;
  cacheRead: string;
  cacheWrite: string;
  cost: string;
  requests: string;
};

const LINE_PALETTE = ["#ff6fa5", "#5aa6f0", "#14b8a6", "#9b7bf0", "#f0a23a", "#e0568a"];

/* 平滑曲线：Catmull-Rom 转三次贝塞尔，得到细腻的发光走势线。 */
function smoothPath(pts: { x: number; y: number }[]): string {
  if (pts.length < 2) return pts.length ? `M ${pts[0].x},${pts[0].y}` : "";
  let d = `M ${pts[0].x.toFixed(2)},${pts[0].y.toFixed(2)}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const cp1x = p1.x + (p2.x - p0.x) / 6;
    const cp1y = p1.y + (p2.y - p0.y) / 6;
    const cp2x = p2.x - (p3.x - p1.x) / 6;
    const cp2y = p2.y - (p3.y - p1.y) / 6;
    d += ` C ${cp1x.toFixed(2)},${cp1y.toFixed(2)} ${cp2x.toFixed(2)},${cp2y.toFixed(2)} ${p2.x.toFixed(2)},${p2.y.toFixed(2)}`;
  }
  return d;
}

function Metric({ label, value, cls }: { label: string; value: string; cls: string }) {
  return (
    <div className={`gc-metric ${cls}`}>
      <span className="gcm-dot" />
      <div className="gcm-text">
        <span className="gcm-label">{label}</span>
        <span className="gcm-value">{value}</span>
      </div>
    </div>
  );
}

/* 发光的 7 天 Token 走势图：每个模型一条彩色曲线，选中某天后上方读出当日合计明细。 */
export function TrendChart({
  data,
  modelTrends,
  emptyLabel,
  labels,
  formatCost,
  modelName,
}: {
  data: DayTokens[];
  modelTrends: ModelDaySeries[];
  emptyLabel: string;
  labels: TrendLabels;
  formatCost: (n: number) => string;
  modelName?: (model: string) => string;
}) {
  const [sel, setSel] = React.useState<number | null>(null);
  const hasData = data.some((d) => d.total_tokens > 0);
  if (!hasData || data.length === 0) {
    return <div className="chart-empty">{emptyLabel}</div>;
  }

  const todayIdx = data.length - 1;
  const active = sel ?? todayIdx;
  const day = data[active];

  const W = 320;
  const H = 188;
  const padX = 8;
  const padTop = 16;
  const padBot = 10;
  const span = data.length > 1 ? data.length - 1 : 1;
  const xAt = (i: number) => padX + (i / span) * (W - padX * 2);

  // 每个模型一条线（取前 6 个）；没有 per-model 数据时回退到总合计单线。
  // 映射成展示名；过滤掉无法映射的随机路由 id（已删除模型的历史数据）。
  const lines = (modelTrends ?? [])
    .filter((l) => l.daily.length === data.length)
    .map((l) => ({ name: modelName ? modelName(l.model) : l.model, daily: l.daily }))
    .filter((l) => !/^neko-model-[0-9a-f]+$/i.test(l.name))
    .slice(0, 6);
  const useModel = lines.length > 0;
  const allValues = useModel ? lines.flatMap((l) => l.daily) : data.map((d) => d.total_tokens);
  const max = Math.max(1, ...allValues);
  const yAt = (v: number) => padTop + (1 - v / max) * (H - padTop - padBot);

  const lineData = useModel
    ? lines.map((l, idx) => ({
        model: l.name,
        color: LINE_PALETTE[idx % LINE_PALETTE.length],
        pts: l.daily.map((v, i) => ({ x: xAt(i), y: yAt(v) })),
      }))
    : [
        {
          model: labels.total,
          color: "#8ab4ff",
          pts: data.map((d, i) => ({ x: xAt(i), y: yAt(d.total_tokens) })),
        },
      ];

  return (
    <div className="glow-chart">
      <div className="gc-readout">
        <div className="gc-main">
          <span className="gc-day">
            {day.date.slice(5).replace("-", "/")}
            {active === todayIdx ? <em className="gc-today-tag">·</em> : null}
          </span>
          <span className="gc-total">{formatTokens(day.total_tokens)}</span>
          <span className="gc-total-label">
            {labels.total} · {day.requests} {labels.requests}
          </span>
        </div>
        <div className="gc-metrics">
          <Metric label={labels.input} value={formatTokens(day.input_tokens)} cls="m-in" />
          <Metric label={labels.output} value={formatTokens(day.output_tokens)} cls="m-out" />
          <Metric label={labels.cacheRead} value={formatTokens(day.cache_read_tokens)} cls="m-cr" />
          <Metric label={labels.cacheWrite} value={formatTokens(day.cache_write_tokens)} cls="m-cw" />
          <Metric label={labels.cost} value={formatCost(day.cost_usd)} cls="m-cost" />
        </div>
      </div>
      <div className="gc-plot" onMouseLeave={() => setSel(null)}>
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="gc-svg">
          <defs>
            <filter id="gcGlow" x="-30%" y="-60%" width="160%" height="220%">
              <feGaussianBlur stdDeviation="2.4" result="b" />
              <feMerge>
                <feMergeNode in="b" />
                <feMergeNode in="SourceGraphic" />
              </feMerge>
            </filter>
          </defs>
          {lineData.map((ld) => (
            <path
              key={ld.model}
              d={smoothPath(ld.pts)}
              fill="none"
              stroke={ld.color}
              strokeWidth={2}
              strokeLinecap="round"
              strokeLinejoin="round"
              filter="url(#gcGlow)"
              className="gc-line"
            />
          ))}
          <line x1={xAt(active)} y1={padTop - 10} x2={xAt(active)} y2={H} className="gc-vline" />
          {lineData.map((ld) => (
            <circle
              key={ld.model}
              cx={ld.pts[active].x}
              cy={ld.pts[active].y}
              r={3}
              fill={ld.color}
              stroke="#fff"
              strokeWidth={1.2}
              className="gc-dot-m"
            />
          ))}
          {data.map((d, i) => (
            <rect
              key={d.date}
              x={Math.max(0, xAt(i) - (W - padX * 2) / span / 2)}
              y={0}
              width={(W - padX * 2) / span}
              height={H}
              fill="transparent"
              onMouseEnter={() => setSel(i)}
            />
          ))}
        </svg>
        <div className="gc-axis">
          {data.map((d, i) => (
            <span
              key={d.date}
              className={i === active ? "active" : ""}
              onMouseEnter={() => setSel(i)}
            >
              {d.date.slice(5).replace("-", "/")}
            </span>
          ))}
        </div>
      </div>
      {useModel ? (
        <div className="gc-legend">
          {lineData.map((ld) => (
            <span key={ld.model} className="gcl-item">
              <span className="gcl-dot" style={{ background: ld.color }} />
              {ld.model}
            </span>
          ))}
        </div>
      ) : null}
    </div>
  );
}
