import type ja from "./locales/ja";

/**
 * 文言キーを日本語リソースから型付けする。存在しないキーを渡すと
 * `npm run typecheck` で落ちる(人が触っても壊れないように — ADR-0002)。
 */
declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "common";
    resources: typeof ja;
  }
}
