import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { useT } from "../../i18n";
import { fmtCost, fmtCompact } from "../formatters";
import { Panel, AsyncBlock } from "../StateBlock";

/// 「哪个 CLI、哪个会话在烧钱」。
///
/// 内核的 stats[] 只能按渠道和模型聚合 —— 所有 CLI 共用一个 API token，
/// 内核分不出是谁发的。维度只能来自代理记录（它认得 User-Agent 和会话头），
/// 成本数字则只认内核日志（代理不会算钱）。两边按「时间 + 模型」配对，
/// 规则和日志页的会话列同一套（lib/sessionMatch.ts / commands::cli_proxy）。
///
/// 没开代理接管时这里全空，那不是故障 —— 提示语要指向开关，而不是让用户
/// 以为数据丢了。
export function CliUsageBreakdown() {
  const t = useT();
  const usage = useQuery({
    queryKey: ["cli-proxy-usage"],
    queryFn: api.cliProxyUsage,
    refetchInterval: 30_000,
    placeholderData: (prev) => prev,
  });

  const report = usage.data;
  const rows = report?.by_cli ?? [];
  const sessions = report?.by_session ?? [];

  return (
    <Panel
      title={t("CLI 消耗")}
      hint={t("代理记录 × 内核日志配对 · 今日")}
      right={
        report && report.unmatched > 0 ? (
          <span
            className="text-[11px] text-muted"
            title={t("代理转发过、但没能在内核日志里找到对应记录的请求数（失败请求或日志页翻页之外）")}
          >
            {t("未配对 {n}", { n: report.unmatched })}
          </span>
        ) : undefined
      }
    >
      <AsyncBlock
        isLoading={usage.isPending}
        error={usage.error}
        isEmpty={rows.length === 0}
        emptyText={t("还没有经代理的请求")}
        emptyHint={t(
          "这个维度只有走本地代理的请求才有：CLI 接管页勾上「经本地代理接管」并对每家点一次写入。",
        )}
      >
        <div className="grid gap-5 lg:grid-cols-2">
          <div>
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-[11px] text-muted">
                  <th className="px-2 py-1.5 font-normal">{t("CLI")}</th>
                  <th className="px-2 py-1.5 text-right font-normal">{t("请求")}</th>
                  <th className="px-2 py-1.5 text-right font-normal">{t("会话")}</th>
                  <th className="px-2 py-1.5 text-right font-normal">out</th>
                  <th className="px-2 py-1.5 text-right font-normal">{t("费用")}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <tr key={r.cli} className="border-b border-border/40">
                    <td className="px-2 py-1.5 font-mono text-xs">{r.cli}</td>
                    <td className="px-2 py-1.5 text-right text-xs tabular-nums text-muted">
                      {r.requests}
                    </td>
                    <td className="px-2 py-1.5 text-right text-xs tabular-nums text-muted">
                      {r.sessions}
                    </td>
                    <td className="px-2 py-1.5 text-right text-xs tabular-nums text-muted">
                      {fmtCompact(r.output_tokens)}
                    </td>
                    <td className="px-2 py-1.5 text-right text-xs tabular-nums">
                      {fmtCost(r.cost)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div>
            {sessions.length === 0 ? (
              <p className="px-2 py-4 text-xs text-muted">
                {t("会话明细要等代理对上内核日志后才有。")}
              </p>
            ) : (
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border text-left text-[11px] text-muted">
                    <th className="px-2 py-1.5 font-normal">{t("会话（最贵前 10）")}</th>
                    <th className="px-2 py-1.5 text-right font-normal">{t("请求")}</th>
                    <th className="px-2 py-1.5 text-right font-normal">{t("费用")}</th>
                  </tr>
                </thead>
                <tbody>
                  {sessions.slice(0, 10).map((s) => (
                    <tr key={`${s.cli}/${s.session_id}`} className="border-b border-border/40">
                      <td
                        className="max-w-0 truncate px-2 py-1.5 font-mono text-xs"
                        title={`${s.cli} · ${s.session_id}`}
                      >
                        {s.session_id.slice(0, 8)}
                      </td>
                      <td className="px-2 py-1.5 text-right text-xs tabular-nums text-muted">
                        {s.requests}
                      </td>
                      <td className="px-2 py-1.5 text-right text-xs tabular-nums">
                        {fmtCost(s.cost)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        </div>
      </AsyncBlock>
    </Panel>
  );
}
