import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot } from "react-dom/client";

import { AppShell } from "./app/AppShell.js";
import "./styles.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Local SQLite + in-process collectors: cache, but never stale for long.
      staleTime: 5_000,
      refetchOnWindowFocus: false,
    },
  },
});

const container = document.getElementById("root");
if (!container) throw new Error("#root is missing from index.html");

createRoot(container).render(
  <QueryClientProvider client={queryClient}>
    <AppShell />
  </QueryClientProvider>,
);
