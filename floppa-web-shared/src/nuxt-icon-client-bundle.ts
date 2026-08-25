// Stub for @nuxt/icon's `#build/nuxt-icon-client-bundle` virtual module.
//
// @nuxt/icon is built for Nuxt, where the framework generates this module. Used
// standalone via @nuxt/ui (no Nuxt build step), the virtual is never provided —
// and under vite-plus's Rolldown dep-optimizer the unresolved import surfaces as
// a "Failed to resolve import #build/nuxt-icon-client-bundle" pre-transform error
// (each app's vite.config.ts aliases the virtual to this file, so it resolves in both the
// optimizer and the build pipeline).
//
// We load icons dynamically via iconify (lucide / simple-icons), not from a
// pre-bundled offline set, so `init` is a no-op — identical to what @nuxt/icon
// itself emits when no client bundle is configured (module.mjs: "export function init() {}").
export function init(): void {
  // no offline icon bundle to register — icons resolve dynamically via iconify
}
