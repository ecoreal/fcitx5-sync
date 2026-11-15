import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";

interface Status {
  fcitx5_running: boolean;
  dict_tool_found: boolean;
  remote_dir: string;
  auto_sync_interval_min: number;
  notify_on_sync: boolean;
  dict_exists: boolean;
  history_exists: boolean;
  last_push: string | null;
  last_pull: string | null;
}

interface Settings {
  remote_dir: string;
  auto_sync_interval_min: number;
  notify_on_sync: boolean;
}

function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [remoteDir, setRemoteDir] = useState("");
  const [busy, setBusy] = useState<"push" | "pull" | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(
    null,
  );

  const refresh = useCallback(async () => {
    const s: Status = await invoke("get_status");
    setStatus(s);
    setRemoteDir(s.remote_dir);
  }, []);

  useEffect(() => {
    void refresh();
    const unlistenDone = listen("sync-done", () => void refresh());
    const unlistenError = listen<string>("sync-error", (e) =>
      setMessage({ ok: false, text: e.payload }),
    );
    return () => {
      void unlistenDone.then((f) => f());
      void unlistenError.then((f) => f());
    };
  }, [refresh]);

  async function runSync(kind: "push" | "pull") {
    setBusy(kind);
    setMessage(null);
    try {
      const text: string = await invoke(
        kind === "push" ? "push_now" : "pull_now",
      );
      setMessage({ ok: true, text });
    } catch (e) {
      setMessage({ ok: false, text: String(e) });
    } finally {
      setBusy(null);
      await refresh();
    }
  }

  async function saveSettings() {
    const settings: Settings = {
      remote_dir: remoteDir.trim(),
      auto_sync_interval_min: status?.auto_sync_interval_min ?? 0,
      notify_on_sync: status?.notify_on_sync ?? true,
    };
    try {
      await invoke("set_settings", { settings });
      setMessage({ ok: true, text: "设置已保存" });
    } catch (e) {
      setMessage({ ok: false, text: String(e) });
    }
    await refresh();
  }

  return (
    <main className="mx-auto flex min-h-screen max-w-md flex-col gap-4 bg-background p-4 text-foreground">
      <h1 className="text-lg font-semibold">Fcitx5 词库同步</h1>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">环境</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-wrap gap-2 text-sm">
          <Badge variant={status?.fcitx5_running ? "default" : "secondary"}>
            fcitx5 {status?.fcitx5_running ? "运行中" : "未运行"}
          </Badge>
          <Badge variant={status?.dict_tool_found ? "default" : "destructive"}>
            libime 工具 {status?.dict_tool_found ? "就绪" : "缺失"}
          </Badge>
          <Badge variant={status?.dict_exists ? "default" : "secondary"}>
            user.dict {status?.dict_exists ? "✓" : "无"}
          </Badge>
          <Badge variant={status?.history_exists ? "default" : "secondary"}>
            user.history {status?.history_exists ? "✓" : "无"}
          </Badge>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">同步</CardTitle>
          <CardDescription className="text-xs">
            推送：本机词库合并进同步目录；拉取：同步目录写回本机（自动备份 +
            重载 fcitx5）
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <div className="flex gap-2">
            <Button
              className="flex-1"
              disabled={busy !== null}
              onClick={() => void runSync("push")}
            >
              {busy === "push" ? "推送中…" : "立即推送"}
            </Button>
            <Button
              className="flex-1"
              variant="outline"
              disabled={busy !== null}
              onClick={() => void runSync("pull")}
            >
              {busy === "pull" ? "拉取中…" : "立即拉取"}
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            上次推送：{status?.last_push ?? "—"} · 上次拉取：
            {status?.last_pull ?? "—"}
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">设置</CardTitle>
          <CardDescription className="text-xs">
            填一个同步盘目录（Syncthing / 坚果云同步文件夹 / U 盘皆可）
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <Input
            placeholder="/home/xxx/SyncDrive/fcitx5"
            value={remoteDir}
            onChange={(e) => setRemoteDir(e.target.value)}
          />
          <Button variant="secondary" onClick={() => void saveSettings()}>
            保存设置
          </Button>
        </CardContent>
      </Card>

      {message && (
        <p
          className={`text-sm ${message.ok ? "text-emerald-600" : "text-red-600"}`}
        >
          {message.text}
        </p>
      )}
    </main>
  );
}

export default App;
