import { createTheme } from "@mantine/core";

/** Dense, calm desktop look: system fonts, small radius, compact controls. */
export const appTheme = createTheme({
  primaryColor: "indigo",
  defaultRadius: "sm",
  fontFamily:
    'system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", "Noto Sans", "Liberation Sans", Arial, sans-serif',
  fontFamilyMonospace: 'ui-monospace, SFMono-Regular, "Cascadia Mono", "Segoe UI Mono", Menlo, Consolas, monospace',
  fontSizes: { xs: "12px", sm: "13px", md: "14px", lg: "16px", xl: "18px" },
  cursorType: "pointer",
  components: {
    Tooltip: { defaultProps: { withArrow: true, openDelay: 400 } },
    Modal: { defaultProps: { centered: true, overlayProps: { backgroundOpacity: 0.35, blur: 1 } } },
    TextInput: { defaultProps: { size: "sm" } },
    PasswordInput: { defaultProps: { size: "sm" } },
    NumberInput: { defaultProps: { size: "sm" } },
    Select: { defaultProps: { size: "sm" } },
    Button: { defaultProps: { size: "sm" } },
  },
});
