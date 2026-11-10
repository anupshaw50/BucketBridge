import { createTheme, type MantineColorsTuple } from "@mantine/core";

/** The brand blue of the logo and the website (#2563eb at shade 6). */
const brand: MantineColorsTuple = [
  "#eff6ff",
  "#dbeafe",
  "#bfdbfe",
  "#93c5fd",
  "#60a5fa",
  "#3b82f6",
  "#2563eb",
  "#1d4ed8",
  "#1e40af",
  "#1e3a8a",
];

/**
 * Dark scheme surfaces tinted towards the brand navy instead of neutral grey.
 * Mantine reads text from 0, borders from 4, surfaces from 6 and the body from 7.
 */
const dark: MantineColorsTuple = [
  "#d6dfec",
  "#b4c1d5",
  "#8e9db6",
  "#66768f",
  "#344560",
  "#26354c",
  "#1a2638",
  "#111a29",
  "#0c1420",
  "#080d16",
];

/** Dense, calm desktop look: system fonts, small radius, compact controls. */
export const appTheme = createTheme({
  primaryColor: "brand",
  primaryShade: { light: 6, dark: 5 },
  colors: { brand, dark },
  defaultRadius: "sm",
  fontFamily:
    'system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", "Noto Sans", "Liberation Sans", "PingFang SC", "Microsoft YaHei", Arial, sans-serif',
  fontFamilyMonospace: 'ui-monospace, SFMono-Regular, "Cascadia Mono", "Segoe UI Mono", Menlo, Consolas, monospace',
  fontSizes: { xs: "12px", sm: "13px", md: "14px", lg: "16px", xl: "18px" },
  cursorType: "pointer",
  components: {
    Tooltip: { defaultProps: { withArrow: true, openDelay: 400 } },
    Modal: { defaultProps: { centered: true, overlayProps: { backgroundOpacity: 0.45, blur: 2 } } },
    TextInput: { defaultProps: { size: "sm" } },
    PasswordInput: { defaultProps: { size: "sm" } },
    NumberInput: { defaultProps: { size: "sm" } },
    Select: { defaultProps: { size: "sm" } },
    Button: { defaultProps: { size: "sm" } },
  },
});
