import { create } from "zustand";
import { persist } from "zustand/middleware";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";

export const useOnboardingStore = create<{ dismissed: boolean; dismiss(): void }>()(
  persist((set) => ({ dismissed: false, dismiss: () => set({ dismissed: true }) }), {
    name: isPreviewFixture() ? "yukinal.onboarding.v1.preview" : "yukinal.onboarding.v1",
  }),
);
