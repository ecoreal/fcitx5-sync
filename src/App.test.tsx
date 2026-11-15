import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import App from "./App";
import { Button } from "@/components/ui/button";

const defaultStatus = {
  fcitx5_running: true,
  dict_tool_found: true,
  remote_dir: "/tmp/sync",
  auto_sync_interval_min: 0,
  notify_on_sync: true,
  dict_exists: true,
  history_exists: true,
  last_push: null,
  last_pull: null,
};

describe("App", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_status") return defaultStatus;
      if (cmd === "get_settings")
        return { remote_dir: "/tmp/sync", auto_sync_interval_min: 0, notify_on_sync: true };
      return null;
    });
  });

  it("renders environment badges and sync buttons", async () => {
    render(<App />);
    expect(await screen.findByText(/fcitx5 运行中/)).toBeTruthy();
    expect(screen.getByText(/libime 工具 就绪/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "立即推送" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "立即拉取" })).toBeTruthy();
  });

  it("push button reports sync result", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_status") return defaultStatus;
      if (cmd === "push_now") return "已推送 user.dict、user.history";
      return null;
    });
    render(<App />);
    const btn = await screen.findByRole("button", { name: "立即推送" });
    fireEvent.click(btn);
    expect(await screen.findByText(/已推送/)).toBeTruthy();
  });

  it("still renders shadcn Button", () => {
    render(<Button>click</Button>);
    expect(screen.getByRole("button", { name: "click" })).toBeTruthy();
  });
});
