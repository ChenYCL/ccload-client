import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { AlertTriangle, Check, Radar, Server } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { TextInput } from "../components/ui/Input";
import type { ChannelSpec, MixOutcome, ProbeResult } from "../types";

/// 本地混用。
///
/// 场景：内核跑在远端（VPS / HF Space），但本机还跑着 cursor2api 之类的
/// OpenAI 兼容服务，想两边一起用。
///
/// 直觉做法是去远端后台加一个渠道、地址填 `http://127.0.0.1:3000` —— 这一定
/// 不通，而且失败方式很迷惑：远端内核解析这个地址得到的是**它自己**的回环口。
/// 拓扑上唯一能同时看见「本机服务」和「公网远端」的位置是本机，所以正确形状是
/// 反过来：本机跑内核，把远端 ccLoad 当成它的一个渠道。理由写在后端
/// `services/local_mix.rs` 的模块注释里。

const LOCAL_PLACEHOLDER = "http://127.0.0.1:3000";

export function LocalMixPage() {
  const t = useT();
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });

  const cfg = settings.data?.kernel;
  const isRemote = cfg?.mode === "remote";
  const running = kernel.data?.state === "running";

  const [localUrl, setLocalUrl] = useState(LOCAL_PLACEHOLDER);
  const [localKey, setLocalKey] = useState("");
  const [localName, setLocalName] = useState("cursor2api");
  const [remoteUrl, setRemoteUrl] = useState("");
  const [remoteKey, setRemoteKey] = useState("");
  const [remoteName, setRemoteName] = useState("remote-ccload");
  const [message, setMessage] = useState<string | null>(null);

  // 远端地址预填成当前设置里的那个 —— 用户切到本机模式之后就看不到它了，
  // 让他再去翻一遍纯属找茬。
  const remoteDefault = cfg?.remote_url ?? "";
  const remoteShown = remoteUrl || remoteDefault;

  const probeLocal = useMutation({
    mutationFn: () => api.localMixProbe(localUrl, localKey || undefined),
    onError: (e) => setMessage(errText(e)),
  });
  const probeRemote = useMutation({
    mutationFn: () => api.localMixProbe(remoteShown, remoteKey || undefined),
    onError: (e) => setMessage(errText(e)),
  });

  const setup = useMutation({
    mutationFn: () => {
      const chans: ChannelSpec[] = [];
      if (probeRemote.data?.ok) {
        chans.push({
          name: remoteName,
          baseUrl: probeRemote.data.base_url,
          apiKey: remoteKey,
          models: probeRemote.data.models,
          // 远端优先：它是你原本的主力，本地服务通常是补充。
          priority: 100,
        });
      }
      if (probeLocal.data?.ok) {
        chans.push({
          name: localName,
          baseUrl: probeLocal.data.base_url,
          apiKey: localKey,
          models: probeLocal.data.models,
          priority: 90,
        });
      }
      return api.localMixSetup(chans);
    },
    onSuccess: (rs: MixOutcome[]) =>
      setMessage(
        rs
          .map((r) =>
            r.ok
              ? `${r.name}：${t("已建为渠道")} #${r.channel_id}`
              : `${r.name}：${t("失败")} —— ${r.error}`,
          )
          .join("\n"),
      ),
    onError: (e) => setMessage(errText(e)),
  });

  const ready = probeLocal.data?.ok || probeRemote.data?.ok;

  return (
    <div>
      <h1 className="t-display">{t("本地混用")}</h1>
      <p className="mt-1 text-sm text-muted">
        {t(
          "把本机跑的 OpenAI 兼容服务（cursor2api 之类）和远端 ccLoad 拼进同一个渠道池，CLI 只认一个地址。",
        )}
      </p>

      {/* 这一段是整页最重要的内容：不讲清楚拓扑，用户会继续去远端后台填
          127.0.0.1，然后对着「连接被拒」查半天自己的服务。 */}
      <div className="mt-4 rounded-xl border border-amber-500/40 bg-amber-500/10 p-3 text-xs leading-relaxed text-amber-900">
        <div className="flex items-center gap-1.5 font-medium">
          <AlertTriangle className="h-3.5 w-3.5" />
          {t("为什么不能直接去远端后台加一条 127.0.0.1 的渠道")}
        </div>
        <p className="mt-1.5">
          {t(
            "远端内核解析 127.0.0.1 得到的是它自己那台机器的回环口，不是你的电脑 —— 它会去连自己的端口然后报连接被拒。地址看着没错，日志也不会告诉你拓扑搞反了。",
          )}
        </p>
        <p className="mt-1.5">
          {t(
            "能同时看见「本机服务」和「公网远端」的位置只有你这台机器。所以正确的接法是反过来：本机跑内核，把远端 ccLoad 当成它的一个渠道，cursor2api 当成另一个，CLI 指向本机内核。故障转移、协议转换、成本统计照旧由内核完成。",
          )}
        </p>
        <p className="mt-1.5">
          {t("另一条路是把本机服务用隧道暴露到公网（ngrok / cloudflared / frp），远端才够得着 —— 代价是服务上了公网，还多一个要维护的东西。")}
        </p>
      </div>

      {/* 前置条件：内核必须是本机模式且已启动，否则建出来的渠道会落到远端。 */}
      {isRemote && (
        <div className="mt-3 rounded-xl border border-red-300 bg-red-50 px-3 py-2 text-xs text-red-700">
          {t(
            "当前内核是「远端」模式。这一页会把渠道建到你连着的那个内核上 —— 也就是远端 —— 结果还是够不着本机服务。请先去「设置」把内核切成「本机内核」并启动，再回来。",
          )}
        </div>
      )}
      {!isRemote && !running && (
        <div className="mt-3 rounded-xl border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-900">
          {t("本机内核还没启动。左下角「启动内核」之后再建渠道。")}
        </div>
      )}

      <div className="mt-5 grid gap-3 lg:grid-cols-2">
        <SourceCard
          icon={<Server className="h-4 w-4 text-accent" />}
          title={t("本机服务")}
          hint={t("cursor2api 等 OpenAI 兼容服务。填根地址即可，结尾的 /v1 会自动去掉。")}
          name={localName}
          onName={setLocalName}
          url={localUrl}
          onUrl={setLocalUrl}
          urlPlaceholder={LOCAL_PLACEHOLDER}
          apiKey={localKey}
          onApiKey={setLocalKey}
          probe={probeLocal.data}
          probing={probeLocal.isPending}
          onProbe={() => probeLocal.mutate()}
        />
        <SourceCard
          icon={<Server className="h-4 w-4 text-accent" />}
          title={t("远端 ccLoad")}
          hint={t("你原来连的那个实例。令牌填它的客户端 API 令牌，不是管理密码。")}
          name={remoteName}
          onName={setRemoteName}
          url={remoteShown}
          onUrl={setRemoteUrl}
          urlPlaceholder="https://xxx.hf.space"
          apiKey={remoteKey}
          onApiKey={setRemoteKey}
          probe={probeRemote.data}
          probing={probeRemote.isPending}
          onProbe={() => probeRemote.mutate()}
        />
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-2">
        <button
          onClick={() => setup.mutate()}
          disabled={setup.isPending || !ready || isRemote || !running}
          title={
            !ready
              ? t("先探测至少一边，拿到模型清单才能建渠道")
              : isRemote
                ? t("先把内核切成本机模式")
                : undefined
          }
          className="flex items-center gap-1 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          <Check className="h-4 w-4" />
          {setup.isPending ? t("建立中…") : t("建成本机内核的渠道")}
        </button>
        <span className="text-xs text-muted">
          {t("建完记得回「CLI 接管」重新写入一次，让 CLI 指向本机内核。")}
        </span>
      </div>

      {message && <p className="mt-4 whitespace-pre-line text-sm text-accent">{message}</p>}
    </div>
  );
}

function SourceCard(props: {
  icon: React.ReactNode;
  title: string;
  hint: string;
  name: string;
  onName: (v: string) => void;
  url: string;
  onUrl: (v: string) => void;
  urlPlaceholder: string;
  apiKey: string;
  onApiKey: (v: string) => void;
  probe?: ProbeResult;
  probing: boolean;
  onProbe: () => void;
}) {
  const t = useT();
  return (
    <div className="card p-4">
      <div className="flex items-center gap-1.5 text-sm font-medium">
        {props.icon}
        {props.title}
      </div>
      <p className="mt-1 text-xs text-muted">{props.hint}</p>

      <label className="mt-3 block text-xs">
        <div className="text-muted">{t("渠道名")}</div>
        <TextInput mono value={props.name} onChange={(e) => props.onName(e.target.value)} className="mt-1" />
      </label>
      <label className="mt-2 block text-xs">
        <div className="text-muted">{t("根地址")}</div>
        <TextInput
          mono
          value={props.url}
          onChange={(e) => props.onUrl(e.target.value)}
          placeholder={props.urlPlaceholder}
          className="mt-1"
        />
      </label>
      <label className="mt-2 block text-xs">
        <div className="text-muted">{t("API 令牌（没有就留空）")}</div>
        <TextInput
          mono
          type="password"
          value={props.apiKey}
          onChange={(e) => props.onApiKey(e.target.value)}
          className="mt-1"
        />
      </label>

      <button
        onClick={props.onProbe}
        disabled={props.probing || !props.url.trim()}
        title={t("问一次 /v1/models，确认通不通，顺带把模型清单带回来")}
        className="mt-3 flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
      >
        <Radar className="h-3.5 w-3.5" />
        {props.probing ? t("探测中…") : t("探测")}
      </button>

      {/* 探测结果要说清「通没通」和「有哪些模型」——后者正是建渠道必填的，
          内核对 models 的要求是 min=1，空清单建不出来。 */}
      {props.probe && (
        <div
          className={cn(
            "mt-2 rounded-lg px-2.5 py-1.5 text-[11px]",
            props.probe.ok ? "bg-emerald-500/10 text-emerald-800" : "bg-red-500/10 text-red-700",
          )}
        >
          {props.probe.ok ? (
            <>
              <div>
                {t("通了 · {n} 个模型", { n: props.probe.models.length })} ·{" "}
                <span className="font-mono">{props.probe.base_url}</span>
              </div>
              <div className="mt-0.5 break-all text-muted">
                {props.probe.models.slice(0, 8).join("、")}
                {props.probe.models.length > 8 && ` …`}
              </div>
            </>
          ) : (
            <div className="break-all">{props.probe.error}</div>
          )}
        </div>
      )}
    </div>
  );
}
