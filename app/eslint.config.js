// @ts-check
import js from "@eslint/js";
import prettier from "eslint-config-prettier";
import jsxA11y from "eslint-plugin-jsx-a11y";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import tseslint from "typescript-eslint";

/**
 * ADR-0002 の層規約を lint で強制する。
 *
 * 文章だけの規約は必ず破られるので、機械で落とす:
 *   - 副作用(データ取得・変更・グローバル状態)を持てるのは organisms 以上だけ
 *   - 下の層は上の層を知らない
 *   - コンポーネントのディレクトリ内部へは直接 import しない(公開面は index.ts のみ)
 *   - Tauri の invoke は lib/api だけが使える
 *
 * 注意: no-restricted-imports は後勝ちで上書きされるため、層ごとに
 * 「その層で禁じるもの全部」を1つの配列に組み立てて渡すこと。
 */

/** コンポーネントの私物ファイルへの直接 import 禁止(全ファイル共通)。 */
const DEEP_IMPORT = {
  // atoms/ui は shadcn が平置きで吐く第三者由来のソースなので対象外
  group: ["@/components/*/*/*", "!@/components/atoms/ui/*"],
  message:
    "コンポーネントの内部ファイルは私物。ディレクトリの index.ts から import する(ADR-0002)。",
};

/** Tauri command の入口を lib/api へ一本化する。通常APIの convertFileSrc 等は許す。 */
const DIRECT_INVOKE = [
  {
    name: "@tauri-apps/api/core",
    importNames: ["invoke"],
    message: "Tauri command は src/lib/api のラッパを経由する(ADR-0002)。",
  },
];

/** locale へ出すべき日本語を JSX 内だけで検出する。コメントや正規表現は対象外。 */
const JAPANESE_UI_LITERALS = [
  {
    selector: "JSXText[value=/[ぁ-んァ-ヶ一-龠々ー]/]",
    message: "JSX の日本語文言は src/i18n/locales の locale から取得する。",
  },
  {
    selector: "JSXAttribute > Literal[value=/[ぁ-んァ-ヶ一-龠々ー]/]",
    message: "JSX 属性の日本語文言は src/i18n/locales の locale から取得する。",
  },
  {
    selector: "JSXExpressionContainer Literal[value=/[ぁ-んァ-ヶ一-龠々ー]/]",
    message: "JSX 式内の日本語文言は src/i18n/locales の locale から取得する。",
  },
  {
    selector: "JSXExpressionContainer TemplateElement[value.raw=/[ぁ-んァ-ヶ一-龠々ー]/]",
    message: "JSX 式内の日本語文言は src/i18n/locales の locale から取得する。",
  },
];

/** TSX の色は design token に寄せる。.ts の canvas fallback は対象外。 */
const RAW_TSX_HEX = [
  {
    selector: "Literal[value=/#[0-9A-Fa-f]{3,8}\\b/]",
    message: "TSX の色は design token を使い、生の hex を書かない。",
  },
  {
    selector: "TemplateElement[value.raw=/#[0-9A-Fa-f]{3,8}\\b/]",
    message: "TSX の色は design token を使い、生の hex を書かない。",
  },
];

/** 副作用(取得・変更・グローバル状態)。organisms 以上でのみ許す。 */
const SIDE_EFFECTS = [
  {
    group: ["@tanstack/react-query"],
    message: "データ取得は organisms 以上の層で行う(ADR-0002)。props で受けること。",
  },
  {
    group: ["zustand", "@/lib/stores/**"],
    message: "グローバル状態に触れるのは organisms 以上(ADR-0002)。props で受けること。",
  },
  {
    group: ["@/lib/queries/**"],
    message: "クエリフックの呼び出しは organisms 以上(ADR-0002)。",
  },
];

/** ドメイン型を知ってはいけない層(atoms / templates)向け。 */
const DOMAIN_TYPES = [
  {
    group: ["@/lib/bindings"],
    message: "この層はドメイン型(Note/Tag/Favorite 等)を知らない(ADR-0002)。",
  },
];

/** 指定した上位層への参照を禁じる。 */
const upper = (...layers) =>
  layers.map((l) => ({
    group: l === "pages" ? ["@/pages/**"] : [`@/components/${l}/**`],
    message: `下の層から ${l} は参照できない(ADR-0002)。`,
  }));

/** no-restricted-imports は後勝ちなので、共通境界と層固有境界を毎回まとめる。 */
const restrictedImports = (patterns, allowInvoke = false) => [
  "error",
  {
    paths: allowInvoke ? [] : DIRECT_INVOKE,
    patterns: [DEEP_IMPORT, ...patterns],
  },
];

/** 層ごとの設定。共通境界は必ず含める(後勝ち対策)。 */
const layer = (dir, patterns) => ({
  files: [`src/components/${dir}/**/*.{ts,tsx}`],
  rules: {
    "no-restricted-imports": restrictedImports(patterns),
  },
});

export default tseslint.config(
  { ignores: ["dist", "src-tauri", "src/lib/bindings.ts"] },
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  // v7 の configs["recommended-latest"] は eslintrc 形式(plugins が配列)なので
  // flat 版を使う
  reactHooks.configs.flat.recommended,
  jsxA11y.flatConfigs.recommended,

  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    plugins: { "react-refresh": reactRefresh },
    rules: {
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
      // WAI-ARIA APG の Window Splitter は role="separator" にフォーカスを持たせる
      // 正当なパターンなので許可する
      "jsx-a11y/no-noninteractive-tabindex": ["error", { roles: ["tabpanel", "separator"] }],
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_" }],
      // 旧実装で多用していた「投げっぱなしの Promise」を封じる
      "@typescript-eslint/no-floating-promises": "error",
      "@typescript-eslint/consistent-type-imports": ["error", { fixStyle: "inline-type-imports" }],
      "no-restricted-imports": restrictedImports([]),
    },
  },

  // UI 文言と色の直書きは TSX の AST だけを調べ、コメント・正規表現・canvas を誤検出しない。
  {
    files: ["src/**/*.tsx"],
    rules: {
      "no-restricted-syntax": ["error", ...JAPANESE_UI_LITERALS, ...RAW_TSX_HEX],
    },
  },

  // Tauri command を包み、Result・デモ切替・エラー変換を引き受ける唯一の窓口。
  {
    files: ["src/lib/api/**/*.{ts,tsx}"],
    rules: { "no-restricted-imports": restrictedImports([], true) },
  },

  // --- 層の境界(ADR-0002) ---
  layer("atoms", [
    ...SIDE_EFFECTS,
    ...DOMAIN_TYPES,
    ...upper("molecules", "organisms", "templates", "pages"),
  ]),
  layer("molecules", [...SIDE_EFFECTS, ...upper("organisms", "templates", "pages")]),
  layer("organisms", [...upper("templates", "pages")]),
  layer("templates", [...SIDE_EFFECTS, ...DOMAIN_TYPES, ...upper("organisms", "pages")]),

  // shadcn 由来の部品はできるだけ素のまま取り込む
  {
    files: ["src/components/atoms/ui/**/*.tsx"],
    rules: { "react-refresh/only-export-components": "off" },
  },

  {
    files: ["**/*.test.{ts,tsx}", "src/test/**"],
    rules: { "@typescript-eslint/no-non-null-assertion": "off" },
  },

  // 設定ファイルとNodeで動かすlint回帰テストはTypeScript projectの外にいる
  {
    files: ["*.config.{js,ts}", "eslint.config.js", "tests/**/*.mjs"],
    ...tseslint.configs.disableTypeChecked,
  },

  prettier,
);
