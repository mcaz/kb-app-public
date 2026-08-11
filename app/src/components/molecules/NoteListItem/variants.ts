import { tv } from "tailwind-variants";

export const noteListItemVariants = tv({
  slots: {
    root: "w-full cursor-pointer border-l-2 px-3.5 py-2 text-left",
    title: "line-clamp-2 font-semibold",
    snippet: "line-clamp-2 text-[11.5px] text-muted",
    dates: "mt-0.5 text-[10.5px] leading-snug text-muted opacity-85",
  },
  variants: {
    selected: {
      true: { root: "border-l-grow bg-sel" },
      false: { root: "border-l-transparent hover:bg-sel/50" },
    },
  },
  defaultVariants: { selected: false },
});
