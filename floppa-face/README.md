# floppa-face

Admin panel for Floppa VPN (Vue 3 + Nuxt UI v4 + Vite+). It is embedded into the `floppa-server`
binary via `memory-serve`; the views, components, router and stores live in `floppa-web-shared`.

Commands, toolchain and conventions are documented in the root [CLAUDE.md](../CLAUDE.md). In
short: `vp dev` here for the dev server (proxies `/api` to `:3000`), `vp check` / `just check`
before committing.
