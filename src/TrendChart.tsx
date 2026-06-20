import React from "react";
import type { DayTokens } from "./types";
import { formatTokens } from "./providers";

/* Lightweight dependency-free SVG bar chart for the 7-day token trend.
   Animated bars, hover tooltip, today highlighted. */
export function TrendChart({
  data,
  emptyLabel,
}: {
  data: DayTokens[];
  emptyLabel: string;
}) {
  const [hover, setHover] = React.useState<number | null>(null);
  const max = Math.max(1, ...data.map((d) => d.total_tokens));
  const hasData = data.some((d) => d.total_tokens > 0);

  const W = 100; // viewBox units, scales to container
  const H = 46;
  const gap = 2.4;
  const barW = (W - gap * (data.length - 1)) / data.length;

  if (!hasData) {
    return <div className="chart-empty">{emptyLabel}</div>;
  }

  const todayIdx = data.length - 1;

  return (
    <div className="trend">
      <div className="trend-plot">
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="trend-svg">
          <defs>
            <linearGradient id="barGrad" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="#ff9ec4" />
              <stop offset="55%" stopColor="#b79cff" />
              <stop offset="100%" stopColor="#8ecbff" />
            </linearGradient>
            <linearGradient id="barGradToday" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="#5fe3c8" />
              <stop offset="100%" stopColor="#14b8a6" />
            </linearGradient>
          </defs>
          {data.map((d, i) => {
            const h = Math.max(d.total_tokens > 0 ? 1.5 : 0, (d.total_tokens / max) * (H - 4));
            const x = i * (barW + gap);
            const y = H - h;
            const isToday = i === todayIdx;
            const active = hover === i;
            return (
              <rect
                key={d.date}
                x={x}
                y={y}
                width={barW}
                height={h}
                rx={1.1}
                fill={isToday ? "url(#barGradToday)" : "url(#barGrad)"}
                opacity={hover === null || active ? 1 : 0.5}
                className="trend-bar"
                style={{ transformOrigin: `${x + barW / 2}px ${H}px`, animationDelay: `${i * 0.05}s` }}
                onMouseEnter={() => setHover(i)}
                onMouseLeave={() => setHover(null)}
              />
            );
          })}
        </svg>
        {hover !== null ? (
          <div
            className="trend-tip"
            style={{ left: `${((hover + 0.5) / data.length) * 100}%` }}
          >
            <strong>{formatTokens(data[hover].total_tokens)}</strong>
            <span>{data[hover].date.slice(5)} · {data[hover].requests}</span>
          </div>
        ) : null}
      </div>
      <div className="trend-axis">
        {data.map((d, i) => (
          <span key={d.date} className={i === todayIdx ? "today" : ""}>
            {d.date.slice(5).replace("-", "/")}
          </span>
        ))}
      </div>
    </div>
  );
}
