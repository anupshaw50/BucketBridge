# Brand assets

| File | Use |
|---|---|
| `logo.svg` | Mark and `BucketBridge` wordmark (bold sans, "Bridge" in the arch colour) on light backgrounds (arch `#2563EB`) |
| `logo-dark.svg` | The same on dark backgrounds (arch `#60A5FA`) |
| `mark.svg`, `mark-dark.svg` | The mark alone, light and dark |
| `app-icon.svg` | Application icon: the mark on a navy tile (`#0B2545`, arch `#38BDF8`) |

The application icon is also `crates/desktop/icons/icon.svg`. After changing it,
regenerate every platform icon (`.icns`, `.ico`, PNG sizes):

```sh
pnpm tauri icon crates/desktop/icons/icon.svg --output crates/desktop/icons
rm -rf crates/desktop/icons/android crates/desktop/icons/ios
```

The Windows `bb.exe` embeds `crates/desktop/icons/icon.ico` at build time
(`crates/cli/build.rs`). The in-app logo is `ui/src/components/BrandLogo.tsx`
and the favicon is `ui/public/favicon.svg`; keep their colours in step.

`social-preview.png` (1280x640) is the image for the repository's social
preview (GitHub Settings, General, Social preview).
