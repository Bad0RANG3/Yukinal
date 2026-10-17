import { useEffect, useRef, useState, type ReactNode } from "react";

import { isNativeDesktopShell, supportsNativeCaptionSurface, supportsNativeTitlebarControls, updateNativeCaptionSurface, updateNativeWindowContext } from "../lib/ipc.js";
import { useServers } from "../lib/servers.js";
import { useWorkspaceStore } from "../stores/workspace-store.js";
import { readCaptionSurface } from "./caption-surface.js";

/** Native chrome reflects the same workspace state as the content it hosts. */
export function WindowFrame({ children }: { children: ReactNode }) {
  const native = isNativeDesktopShell();
  const captionSurface = native && supportsNativeCaptionSurface();
  const rootRef = useRef<HTMLDivElement>(null);
  const [captionJoined, setCaptionJoined] = useState(false);
  const [maximized, setMaximized] = useState(false);
  const titlebarControls = supportsNativeTitlebarControls() && captionJoined;
  const section = useWorkspaceStore((state) => state.primary);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const agentBusy = useWorkspaceStore((state) => state.agentBusy);
  // Observe the cache; AppShell owns fetching and polling.
  const servers = useServers({ enabled: false });
  const selected = section === "servers"
    ? servers.data?.find((server) => server.id === selectedServerId) : undefined;
  const serverName = selected?.name.replace(/[\u0000-\u001f\u007f]/gu, " ").slice(0, 160).replace(/\p{Surrogate}/gu, "�") ?? null;
  const serverStatus = selected?.status ?? null;

  useEffect(() => {
    if (!native) return;
    void updateNativeWindowContext({ section, serverName, serverStatus, agentBusy }).catch((error: unknown) => {
      // Older running hosts fall back to Activity until a host restart adds Board.
      if (section !== "board") throw error;
      return updateNativeWindowContext({ section: "activity", serverName: null, serverStatus: null, agentBusy });
    })
      .catch((error: unknown) => console.warn("Could not update native window context", error));
  }, [native, section, serverName, serverStatus, agentBusy]);

  useEffect(() => {
    const root = rootRef.current;
    if (!captionSurface || !root) return;
    let frame = 0;
    let previous = "";
    let revision = 0;
    let disposed = false;
    const synchronize = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const surface = readCaptionSurface(root);
        // Height matters for maximize/restore, but repeated native layout
        // notifications with identical bounds must not restart the handshake.
        const serialized = JSON.stringify([surface, window.innerWidth, window.innerHeight]);
        if (serialized === previous) return;
        previous = serialized;
        const request = ++revision;
        // Only remove the HTML corners after native chrome confirms its cap.
        // A rejected bridge update must leave a complete, rounded panel visible.
        void updateNativeCaptionSurface(surface).then((presentation) => {
          if (disposed || request !== revision) return;
          setCaptionJoined(surface !== null);
          if (presentation) setMaximized(presentation.maximized);
        }).catch((error: unknown) => {
          if (disposed || request !== revision) return;
          previous = "";
          setCaptionJoined(false);
          console.warn("Could not update native caption surface", error);
        });
      });
    };
    const resize = new ResizeObserver(synchronize);
    resize.observe(root);
    const panel = root.querySelector(".agent-panel.agent-workspace");
    if (panel) resize.observe(panel);
    const mutation = new MutationObserver(synchronize);
    const shell = root.querySelector(".app-shell");
    if (shell) mutation.observe(shell, { attributes: true, attributeFilter: ["class", "style"] });
    window.addEventListener("resize", synchronize);
    // CSS-only HMR does not resize the panel; repaint the native cap too.
    import.meta.hot?.on("vite:afterUpdate", synchronize);
    synchronize();
    return () => {
      disposed = true;
      cancelAnimationFrame(frame);
      resize.disconnect();
      mutation.disconnect();
      window.removeEventListener("resize", synchronize);
      import.meta.hot?.off("vite:afterUpdate", synchronize);
      void updateNativeCaptionSurface(null).catch(() => undefined);
    };
  }, [captionSurface]);

  if (!native) return children;
  return <div ref={rootRef} role="group" aria-label={maximized ? "最大化工作区" : "工作区"}
    className={`native-workspace${captionJoined ? " native-caption-integrated" : ""}${titlebarControls ? " native-caption-controls" : ""}${maximized ? " native-window-maximized" : ""}`}
  >{children}</div>;
}
