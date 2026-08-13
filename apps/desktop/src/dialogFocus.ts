const focusableSelector = [
  "a[href]",
  "area[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[contenteditable='true']",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

export function dialogFocusTargetIndex(
  currentIndex: number,
  focusableCount: number,
  backwards: boolean,
): number | null {
  if (focusableCount <= 0) return null;
  if (currentIndex < 0) return backwards ? focusableCount - 1 : 0;
  if (backwards && currentIndex === 0) return focusableCount - 1;
  if (!backwards && currentIndex === focusableCount - 1) return 0;
  return null;
}

export function dialogFocusableElements(dialog: HTMLElement): HTMLElement[] {
  return Array.from(dialog.querySelectorAll<HTMLElement>(focusableSelector)).filter((element) => {
    if (element.closest("[hidden], [inert], [aria-hidden='true']")) return false;
    const style = window.getComputedStyle(element);
    return style.display !== "none" && style.visibility !== "hidden";
  });
}

export function topmostModalDialog(): HTMLElement | null {
  const dialogs = Array.from(
    document.querySelectorAll<HTMLElement>("[role='dialog'][aria-modal='true']"),
  ).filter((dialog) => {
    if (dialog.hidden || dialog.closest("[hidden], [inert], [aria-hidden='true']")) return false;
    const style = window.getComputedStyle(dialog);
    return style.display !== "none" && style.visibility !== "hidden";
  });
  return dialogs[dialogs.length - 1] ?? null;
}

export function expandedControlInsideDialog(dialog: HTMLElement): HTMLElement | null {
  const expanded = document.activeElement instanceof HTMLElement
    ? document.activeElement.closest<HTMLElement>("[aria-expanded='true']")
    : null;
  return expanded && dialog.contains(expanded) ? expanded : null;
}
