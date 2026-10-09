// The most recent quick-action / AI result of this palette session, so closing
// the action inspector (or stepping to another row and back) does not throw
// the output away. It is never written to the history: saving stays the
// inspector's explicit Save action. The palette forgets it when it hides, so
// the next invocation starts clean.

type ActionResultMemory = {
  entryId: string | undefined;
  text: string | undefined;
};

export const actionResultState = $state<ActionResultMemory>({
  entryId: undefined,
  text: undefined,
});

export const rememberActionResult = (entryId: string, text: string): void => {
  actionResultState.entryId = entryId;
  actionResultState.text = text;
};

export const rememberedActionResult = (entryId: string | undefined): string | undefined =>
  entryId !== undefined && actionResultState.entryId === entryId
    ? actionResultState.text
    : undefined;

export const forgetActionResult = (): void => {
  actionResultState.entryId = undefined;
  actionResultState.text = undefined;
};
