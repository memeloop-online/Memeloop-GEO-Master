import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Outlet } from "react-router-dom";
import { AppRoutes } from "./app";

const state = vi.hoisted(() => {
  let resolveShell!: () => void;
  const shellReady = new Promise<void>((resolve) => {
    resolveShell = resolve;
  });
  return {
    status: "checking",
    shellReady,
    resolveShell,
    shellResolved: false,
    shellRequested: vi.fn(),
    measurementRequested: vi.fn(),
  };
});

vi.mock("./auth/AuthProvider", () => ({
  useAuth: () => ({
    status: state.status,
    session:
      state.status === "authenticated"
        ? { memberships: [{ tenant_id: "tenant-a" }] }
        : undefined,
  }),
}));
vi.mock("./components/AsyncState", () => ({
  LoadingState: () => <p>Route loading</p>,
  ErrorState: () => <p>Session unavailable</p>,
  UnauthorizedState: () => <p>Permission denied</p>,
}));
vi.mock("./pages/LoginPage", () => ({
  LoginPage: () => <h1>Login</h1>,
}));
vi.mock("./components/AppShell", async () => {
  state.shellRequested();
  await state.shellReady;
  state.shellResolved = true;
  return { AppShell: () => <Outlet /> };
});
vi.mock("./pages/QuestionSetsPage", () => {
  state.measurementRequested();
  return { QuestionSetsPage: () => <h1>Measurement</h1> };
});

afterEach(cleanup);

describe("measurement route preload", () => {
  it.each([
    ["checking", "/app/tenant-a/project-a/measurement", "Route loading"],
    ["anonymous", "/app/tenant-a/project-a/measurement", "Login"],
    ["authenticated", "/app/tenant-a/project-a/reports", "Route loading"],
    [
      "authenticated",
      "/app/tenant-b/project-b/measurement",
      "Permission denied",
    ],
  ])("does not preload for %s at %s", async (status, path, text) => {
    state.status = status;
    render(
      <MemoryRouter initialEntries={[path]}>
        <AppRoutes />
      </MemoryRouter>,
    );
    expect(await screen.findByText(text)).toBeVisible();
    expect(state.measurementRequested).not.toHaveBeenCalled();
  });

  it("requests the authenticated measurement page before the shell resolves", async () => {
    state.status = "checking";
    const view = render(<AppRoutes />, {
      wrapper: ({ children }) => (
        <MemoryRouter
          initialEntries={["/app/tenant-a/project-a/measurement?tab=records"]}
        >
          {children}
        </MemoryRouter>
      ),
    });
    expect(state.measurementRequested).not.toHaveBeenCalled();

    state.status = "authenticated";
    view.rerender(<AppRoutes />);
    await waitFor(() => {
      expect(state.shellRequested).toHaveBeenCalledOnce();
      expect(state.measurementRequested).toHaveBeenCalledOnce();
    });
    expect(state.shellResolved).toBe(false);
    expect(screen.queryByRole("heading", { name: "Measurement" })).toBeNull();

    await act(async () => state.resolveShell());
    expect(
      await screen.findByRole("heading", { name: "Measurement" }),
    ).toBeVisible();
    expect(state.measurementRequested).toHaveBeenCalledOnce();
  });
});
