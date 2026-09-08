import { tv } from "tailwind-variants";

export const appEntryVariants = tv({
  slots: {
    root: "bg-ground text-ink flex min-h-screen items-center justify-center p-8",
    message: "max-w-lg text-center",
    title: "text-xl font-semibold",
    description: "text-muted mt-3 text-sm leading-relaxed",
  },
});
