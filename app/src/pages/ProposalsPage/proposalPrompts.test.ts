import { createInstance } from "i18next";
import { describe, expect, it } from "vitest";

import en from "@/i18n/locales/en/proposals.json";
import ja from "@/i18n/locales/ja/proposals.json";
import { demoProposalTickets } from "@/lib/demo/proposals";

// 2026-09-06: 通常getで未採用を取得できなくなっても、本人の明示レビュー依頼は途切れさせない。
describe("proposal request prompts", () => {
  it.each(["ja", "en"] as const)(
    "%sのレビュー・改訂は専用取得と明示された提案IDを使う",
    async (language) => {
      const i18n = createInstance();
      await i18n.init({
        lng: language,
        resources: { ja: { proposals: ja }, en: { proposals: en } },
        defaultNS: "proposals",
        interpolation: { escapeValue: false },
      });
      const t = i18n.getFixedT(language, "proposals");
      const ticket = demoProposalTickets().find((item) => item.status === "review_pending")!;
      for (const kind of ["review", "revise"] as const) {
        const prompt = t(`prompts.${kind}`, { note: ticket.note_id, uid: ticket.note_uid });
        expect(prompt).toContain("get_proposal");
        expect(prompt).not.toMatch(/\bget\b/);
        expect(prompt).toContain(ticket.note_id);
        expect(prompt).toContain(ticket.note_uid);
        expect(prompt).toContain(`${kind}_proposal`);
        expect(prompt).toContain("etag");
        expect(prompt).not.toContain("{{");
      }
      expect(t("prompts.create")).toContain("create_proposal");
      expect(t("prompts.create")).toContain("get_proposal");
    },
  );
});
