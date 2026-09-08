import { tv } from "tailwind-variants";

export const categoryAccordionVariants = tv({
  slots: {
    message: "text-muted m-0 px-7 py-1 text-[11px]",
    failure: "flex flex-col items-start gap-2 px-3 py-2",
    error: "text-danger m-0 text-[11px] leading-relaxed break-words",
  },
});
