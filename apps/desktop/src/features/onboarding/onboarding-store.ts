import { create } from "zustand";
import { persist } from "zustand/middleware";

export const useOnboardingStore = create<{ dismissed: boolean; dismiss(): void }>()(
  persist((set) => ({ dismissed: false, dismiss: () => set({ dismissed: true }) }), {
    name: "yukinal.onboarding.v1",
  }),
);
