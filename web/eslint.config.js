// web/eslint.config.js — 前端代码质量门禁(ESLint 9/10 flat config)。
//
// 分工:本配置只管「代码质量 / 常见错误」(未用变量、可疑写法、Vue 规则等);
// 类型逃逸 ratchet(any / as never / as unknown as / @ts-expect-error / 非空断言)
// 仍由 tools/check-frontend-lint.mjs 负责,两者并存、互不替代。
//
// 规则集刻意克制:目标是在**不改业务代码**的前提下 `eslint .` 零 error 通过。
// 对存量噪音只在下方 rules 里定向放松(设 warn 或关闭并写明理由),
// 不使用大面积 /* eslint-disable */ 注释。
import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import pluginVue from 'eslint-plugin-vue';
import prettier from 'eslint-config-prettier';

export default tseslint.config(
  // 构建产物、依赖、静态资源不参与检查
  { ignores: ['dist/**', 'node_modules/**', 'coverage/**', 'public/**'] },

  // 1) JS 基础推荐规则
  js.configs.recommended,

  // 2) TypeScript 推荐规则(非 type-checked 版)。
  //    理由:type-checked 需 project service 并显著拖慢 lint;而本仓已有
  //    `vue-tsc --noEmit` 作为独立硬门禁(见 tools/check-all.ps1),类型层不缺覆盖。
  ...tseslint.configs.recommended,

  // 3) Vue SFC 推荐规则
  ...pluginVue.configs['flat/recommended'],

  // 4) .vue 的 <script lang="ts"> 交给 @typescript-eslint/parser
  {
    files: ['**/*.vue'],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
        ecmaVersion: 'latest',
        sourceType: 'module',
      },
    },
    rules: {
      // 本仓 .vue 一律为 <script setup lang="ts">。与 typescript-eslint 的
      // eslint-recommended 对 .ts 的处理保持一致:未定义标识符交给 TS/vue-tsc 判定
      // (浏览器/Node 全局无需在此登记),避免误报。
      'no-undef': 'off',
    },
  },

  // 5) 定向规则(克制;仅压存量噪音,不掩盖真实问题)
  {
    rules: {
      // 未用变量降为 warn:测试与解构中的存量未用绑定较多;命名以 _ 开头视为有意忽略。
      '@typescript-eslint/no-unused-vars': [
        'warn',
        {
          argsIgnorePattern: '^_',
          varsIgnorePattern: '^_',
          caughtErrors: 'none',
          ignoreRestSiblings: true,
        },
      ],
      // 单字组件名在本项目是既有约定(App.vue / Sidebar.vue),不与多词命名规范冲突。
      'vue/multi-word-component-names': 'off',
      // v-html 在本项目用于渲染已消毒的 Markdown/富文本(见 sanitize-html 管线),
      // 属既定实现;保留为 warn 提示风险,不阻断。
      'vue/no-v-html': 'warn',

      // ---- 以下为「存量问题」降级为 warn(2026-09-13 首次接入 ESLint)----
      // 本任务约定不改业务代码,首轮先让 `eslint .` 零 error;这些规则仍有价值,
      // 故只降为 warn 保持可见(而非 off / disable 注释),留待后续批次清偿:
      //   no-control-regex ×3                     src/render.ts(消毒时的 NUL 字符正则,属有意)
      //   no-useless-assignment ×3                src/sandbox/draggable.ts、iframe-lifecycle.ts(死存储)
      //   prefer-const ×2                         src/cssSanitize.ts、src/sandbox/iframe-lifecycle.ts
      //   no-useless-escape ×1                    src/characterScriptSandbox.test.ts
      //   no-case-declarations ×1                 src/sseReducer.ts(case 块内 const)
      //   no-fallthrough ×1                       src/taskStatus.ts('pending' 有意贯穿到 'planned')
      //   vue/no-side-effects-in-computed-properties ×1  src/components/DevToolsModal.vue
      'no-control-regex': 'warn',
      'no-useless-assignment': 'warn',
      'no-useless-escape': 'warn',
      'no-case-declarations': 'warn',
      'no-fallthrough': 'warn',
      'prefer-const': 'warn',
      'vue/no-side-effects-in-computed-properties': 'warn',
    },
  },

  // 6) 关闭与 Prettier 冲突的格式规则(必须放在最后)
  prettier,
);
