/**
 * Test helper for the native C++ desktop host binding.
 *
 * The UI talks to the host only through `window.__YUKINAL_HOST__`; tests install a
 * fake for it instead of mocking the retired Tauri IPC layer.
 */

type Host = NonNullable<Window["__YUKINAL_HOST__"]>;

export function installHost(overrides: Partial<Host> = {}): Host {
  const host: Host = {
    invoke: async () => ({}),
    listen: async () => () => {},
    openUrl: async () => ({}),
    ...overrides,
  };
  (window as unknown as { __YUKINAL_HOST__?: Host }).__YUKINAL_HOST__ = host;
  return host;
}

export function removeHost(): void {
  delete (window as unknown as { __YUKINAL_HOST__?: Host }).__YUKINAL_HOST__;
}
