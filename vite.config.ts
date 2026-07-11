import { defineConfig } from 'vite-plus'

// Root Vite+ config: shared lint (oxlint) + format (oxfmt) for the whole
// bun workspace. Per-package vite.config.ts files hold Vite/framework config.
// Globs are resolved from this root, so use workspace paths.
export default defineConfig({
  staged: {
    '*.{css,js,ts,tsx,vue}': 'vp check --fix',
  },
  lint: {
    plugins: ['eslint', 'typescript', 'unicorn', 'oxc', 'vue'],
    categories: {
      correctness: 'error',
    },
    env: {
      browser: true,
      builtin: true,
    },
    // Auto-generated code — never linted (regenerated from source):
    //   src/client/** = OpenAPI client, bindings.ts = tauri-specta
    ignorePatterns: [
      '**/dist/**',
      '**/dist-ssr/**',
      '**/coverage/**',
      '**/src/client/**',
      '**/bindings.ts',
    ],
    rules: {
      'no-array-constructor': 'error',
      'typescript/ban-ts-comment': 'error',
      'typescript/no-empty-object-type': 'error',
      'typescript/no-explicit-any': 'error',
      'typescript/no-namespace': 'error',
      'typescript/no-require-imports': 'error',
      'typescript/no-unnecessary-type-constraint': 'error',
      'typescript/no-unsafe-function-type': 'error',
    },
    overrides: [
      {
        files: ['**/*.ts', '**/*.tsx', '**/*.mts', '**/*.cts', '**/*.vue'],
        rules: {
          'constructor-super': 'off',
          'getter-return': 'off',
          'no-class-assign': 'off',
          'no-const-assign': 'off',
          'no-dupe-class-members': 'off',
          'no-dupe-keys': 'off',
          'no-func-assign': 'off',
          'no-import-assign': 'off',
          'no-new-native-nonconstructor': 'off',
          'no-obj-calls': 'off',
          'no-redeclare': 'off',
          'no-setter-return': 'off',
          'no-this-before-super': 'off',
          'no-undef': 'off',
          'no-unreachable': 'off',
          'no-unsafe-negation': 'off',
          'no-var': 'error',
          'no-with': 'off',
          'prefer-const': 'error',
          'prefer-rest-params': 'error',
          'prefer-spread': 'error',
        },
      },
    ],
    options: {
      typeAware: true,
      typeCheck: true,
    },
  },
  fmt: {
    semi: false,
    singleQuote: true,
    printWidth: 100,
    sortPackageJson: false,
    // Keep the workspace-level formatter focused on frontend source. Rust manifests,
    // generated SQLx metadata, docs, and workflow YAML have their own formatters.
    ignorePatterns: [
      '.claude/**',
      '.github/**',
      '**/*.html',
      '**/*.json',
      '**/*.json5',
      '**/*.md',
      '**/*.toml',
      '**/src/client/**',
      '**/bindings.ts',
    ],
  },
})
