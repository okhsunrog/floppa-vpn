# Safe Area & Z-Index Architecture

## Problem

The Tauri client runs edge-to-edge on Android (`viewport-fit=cover` in `index.html`). The WebView extends behind the status bar and navigation bar. `env(safe-area-inset-*)` does **not** work in Android WebView, so we use custom CSS variables set from Kotlin.

## How Safe Area Insets Are Set

1. **Kotlin plugin** (`tauri-plugin-vpn/android/.../VpnPlugin.kt` → `getSafeAreaInsets()`) queries `window.decorView.rootWindowInsets` on Android 11+. Returns `max(statusBars.top, cutout.top)` and `navBars.bottom` in density-independent pixels.
2. **Rust bridge** (`tauri-plugin-vpn/src/mobile.rs`) forwards to Kotlin. On non-Android platforms, returns `(0, 0)`.
3. **JavaScript** (`floppa-client/src/main.ts`) sets `--safe-area-inset-top` and `--safe-area-inset-bottom` on `document.documentElement`. On desktop these are never set, so CSS `var()` defaults to `0px`.

## Z-Index Layer Cake

```
Layer                              z-index   Position   Source
─────────────────────────────────────────────────────────────────
Normal page content                auto      static     html padding pushes below safe area
Sticky navbar                      40        sticky     AppLayout.vue header
Safe area masks (::before/::after) 40        fixed      styles.css — colored bars over status/nav bar
Modal/Slideover overlay + content  50        fixed      vite.config.ts global ui override
Dropdowns (Select, SelectMenu)     51        absolute   vite.config.ts — above modals when inside one
```

## How Each Piece Works

### Normal Content Flow (`styles.css`)

```css
html {
  padding-top: var(--safe-area-inset-top, 0px) !important;
  padding-bottom: var(--safe-area-inset-bottom, 0px) !important;
}
```

Pushes all normal document flow below the status bar and above the nav bar.

### Safe Area Masks (`styles.css`)

```css
html::before { /* top mask */ }
html::after  { /* bottom mask */ }
```

Fixed-position colored rectangles (`bg: var(--ui-bg)`) covering the status bar and navigation bar zones at `z-index: 40`. When the user scrolls, content passes behind these masks instead of showing through the transparent system bars.

### Navbar (`AppLayout.vue`)

Sticky header with `z-40`, positioned at `top: var(--safe-area-inset-top, 0px)`. Sits at the same z-level as the masks, both above normal content.

### Modals (`vite.config.ts` + `styles.css`)

Global Nuxt UI theme override in `vite.config.ts`:

```js
ui: {
  modal: { slots: { overlay: 'z-50', content: 'z-50' } },
  slideover: { slots: { overlay: 'z-50', content: 'z-50' } },
}
```

All modals/slideovers automatically render above the navbar and safe area masks.

Modal content max-height is constrained in `styles.css` to avoid extending into safe area zones:

```css
[role='dialog'][data-slot='content']:not(.inset-y-0) {
  max-height: calc(
    100dvh - 2rem - var(--safe-area-inset-top, 0px) - var(--safe-area-inset-bottom, 0px)
  ) !important;
}
```

- `:not(.inset-y-0)` **excludes slideovers** — they span the full viewport height by design (Nuxt UI gives them `inset-y-0` for `side: left/right, inset: false`)
- On desktop, safe area vars are `0px`, so the default Nuxt UI `max-h-[calc(100dvh-2rem)]` is preserved exactly

### Slideovers (`AppLayout.vue`)

Slideovers cover the full viewport (`inset-y-0`) so they are excluded from the max-height rule. They handle safe area padding per-component via `:ui` props:

```vue
:ui="{
  overlay: 'z-50',
  content: 'z-50 pt-[var(--safe-area-inset-top,0px)] pb-[var(--safe-area-inset-bottom,0px)]',
  close: 'top-[calc(1rem+var(--safe-area-inset-top,0px))]',
}"
```

The `z-50` in this per-component override is redundant with the global config but harmless.

## Adding New Modals

No special handling needed. Every `UModal` automatically gets:
- `z-50` overlay and content (above navbar)
- Safe-area-aware max-height (via the global CSS rule)

## Adding New Slideovers

If the slideover needs to look correct on Android, add safe area padding to its content via `:ui`:

```vue
:ui="{
  content: 'pt-[var(--safe-area-inset-top,0px)] pb-[var(--safe-area-inset-bottom,0px)]',
}"
```

## Key Files

- `floppa-client/src/styles.css` — html padding, mask pseudo-elements, modal max-height rule
- `floppa-client/vite.config.ts` — global z-50 override for modals/slideovers
- `floppa-client/src/main.ts` — sets CSS custom properties from Kotlin plugin
- `floppa-client/index.html` — `viewport-fit=cover` meta tag
- `floppa-web-shared/src/components/AppLayout.vue` — navbar (z-40), slideover safe area `:ui`
- `tauri-plugin-vpn/android/.../VpnPlugin.kt` — `getSafeAreaInsets()` Kotlin command
