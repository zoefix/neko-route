import React from "react";

export function Petals({ count = 14 }: { count?: number }) {
  const petals = React.useMemo(
    () =>
      Array.from({ length: count }, (_, i) => {
        const size = 8 + Math.random() * 12;
        const hues = ["#ffd5e6", "#e4d6ff", "#d3f3ec", "#cfe7ff"];
        return {
          left: Math.random() * 100,
          size,
          delay: Math.random() * 16,
          duration: 14 + Math.random() * 14,
          color: hues[i % hues.length],
          sway: (Math.random() - 0.5) * 18,
        };
      }),
    [count],
  );

  return (
    <div className="petals" aria-hidden>
      {petals.map((p, i) => (
        <span
          key={i}
          className="petal"
          style={
            {
              left: `${p.left}%`,
              width: p.size,
              height: p.size,
              animationDelay: `${p.delay}s`,
              animationDuration: `${p.duration}s`,
              background: `radial-gradient(circle at 30% 30%, #fff, ${p.color})`,
              "--sway": `${p.sway}vw`,
            } as React.CSSProperties
          }
        />
      ))}
    </div>
  );
}
