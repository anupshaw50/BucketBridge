import { useComputedColorScheme } from "@mantine/core";

/** Colours of the mark on a light and on a dark background (assets/brand). */
const PALETTE = {
  light: { arch: "#2563EB", bars: ["#111827", "#374151", "#6B7280"], text: "#111827" },
  dark: { arch: "#60A5FA", bars: ["#F9FAFB", "#D1D5DB", "#9CA3AF"], text: "#F9FAFB" },
} as const;

/** The BucketBridge logo: the mark alone, or the mark with the `BucketBridge` wordmark. */
export function BrandLogo({ height = 32, withWordmark = false }: { height?: number; withWordmark?: boolean }) {
  const colors = PALETTE[useComputedColorScheme("light")];
  const width = withWordmark ? 372 : 68;
  const viewHeight = withWordmark ? 74 : 72;
  return (
    <svg
      role="img"
      aria-label="BucketBridge"
      width={(height * width) / viewHeight}
      height={height}
      viewBox={`0 0 ${width} ${viewHeight}`}
    >
      <g transform={withWordmark ? "translate(0 1)" : undefined}>
        <path d="M 6 30.5 A 28 28 0 0 1 62 30.5" fill="none" stroke={colors.arch} strokeWidth={5} />
        <rect x={0} y={36} width={68} height={8} rx={2.5} fill={colors.bars[0]} />
        <rect x={4} y={50} width={60} height={8} rx={2.5} fill={colors.bars[1]} />
        <rect x={9} y={64} width={50} height={8} rx={2.5} fill={colors.bars[2]} />
      </g>
      {withWordmark && (
        <text
          x={96}
          y={56}
          textLength={270}
          lengthAdjust="spacingAndGlyphs"
          fontFamily="Inter, SF Pro Display, -apple-system, Segoe UI, Helvetica Neue, Arial, sans-serif"
          fontSize={44}
          fontWeight={700}
          letterSpacing={-1}
          fill={colors.text}
        >
          Bucket<tspan fill={colors.arch}>Bridge</tspan>
        </text>
      )}
    </svg>
  );
}
