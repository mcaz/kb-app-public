import type { DistillationIssueError } from "@/lib/api";

export function issueErrorKey(error: Exclude<DistillationIssueError, { code: "needs_review" }>) {
  switch (error.code) {
    case "ai":
      return `settings.distillation.failures.${error.kind}` as const;
    case "context_limit":
      return "settings.distillation.failures.context_limit";
    case "context_size_limit":
      return "settings.distillation.failures.context_size_limit";
    case "review_round_limit":
      return "settings.distillation.failures.review_round_limit";
    case "review_no_progress":
      return "settings.distillation.failures.review_no_progress";
    case "not_supported":
      return "settings.distillation.failures.review_not_supported";
    case "review_failed":
      return "settings.distillation.failures.review_failed";
  }
}
