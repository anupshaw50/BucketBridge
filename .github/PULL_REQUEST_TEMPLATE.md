## Summary

<!-- What does this change and why? -->

## Related issue

<!-- Fixes #123 -->

## Checklist

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `pnpm --dir ui typecheck && pnpm --dir ui lint && pnpm --dir ui test && pnpm --dir ui build`
- [ ] `cargo deny check` (when dependencies changed)
- [ ] `docs/desktop-api.md` updated first (when a command or event changed)
- [ ] No em dash characters in code, docs or UI text
