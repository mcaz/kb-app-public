import { tv } from "tailwind-variants";

export const connectPageVariants = tv({
  slots: {
    failure: "flex flex-col items-start gap-3 px-5 py-4",
    error: "text-danger text-sm leading-relaxed",
    description: "text-muted text-xs leading-relaxed",
  },
});
