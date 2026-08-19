import { useCallback, useEffect, useRef, useState } from "react";
import type { LogEntry } from "../../types";

/// 实时日志的「跟随 / 暂停」状态机。
///
/// /admin/logs 按 time 倒序返回，最新的一条在最上面，所以**实时边缘在顶部**：
/// 「跟随」= 贴着顶部，新日志从上面推进来。用户往下滚去看历史的那一刻就必须
/// 停止跟随，否则每 2.5 秒插入的新行会把他正在读的那一行不断往下顶。
///
/// 这里选择的做法是「冻结整份快照」而不是「插入新行并补偿 scrollTop」：
/// 补偿方案在行高不一致时会抖，而且用户读日志时本来也不需要看到新数据 ——
/// 未读条数用一个角标告诉他就够了。回到顶部即恢复跟随。

/** 距顶部这个像素内都算「在实时边缘」。给一点余量，滚轮很难精确停在 0。 */
const FOLLOW_EPS = 24;
/** 新行高亮持续时间，和 LogTable 里的 transition 时长对齐。 */
const FLASH_MS = 1000;
/** 已见 id 集合的上限，超了就用当前窗口重建，避免长时间开着页面无限增长。 */
const SEEN_CAP = 4000;

const NO_IDS: ReadonlySet<number> = new Set();

export type LogFeed = {
  /** 挂到滚动容器上 */
  scrollRef: React.RefObject<HTMLDivElement>;
  onScroll: () => void;
  /** 实际要渲染的列表：跟随时是实时数据，暂停时是冻结快照 */
  logs: LogEntry[];
  flashIds: ReadonlySet<number>;
  following: boolean;
  /** 暂停期间新到的条数 */
  pending: number;
  /** 回到顶部并恢复跟随 */
  resume: () => void;
};

export function useLogFeed(live: LogEntry[], resetKey: string): LogFeed {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [following, setFollowing] = useState(true);
  const [flashIds, setFlashIds] = useState<ReadonlySet<number>>(NO_IDS);

  // 事件回调里要读到最新值，但不该因为它们变化而重建回调。
  const followingRef = useRef(true);
  const liveRef = useRef(live);
  liveRef.current = live;

  const frozenRef = useRef<LogEntry[]>([]);
  const seenRef = useRef<Set<number>>(new Set());
  const primedRef = useRef(false);

  // 筛选条件一变，之前的快照和「已见」集合就都失效了：新结果集里的每一条
  // 对用户来说都是第一次出现，但整页闪一遍等于没有信号，所以是重新 prime。
  useEffect(() => {
    primedRef.current = false;
    seenRef.current = new Set();
    frozenRef.current = [];
    followingRef.current = true;
    setFollowing(true);
    setFlashIds(NO_IDS);
    scrollRef.current?.scrollTo({ top: 0 });
  }, [resetKey]);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const atTop = el.scrollTop <= FOLLOW_EPS;
    if (atTop === followingRef.current) return;
    followingRef.current = atTop;
    // 离开顶部的瞬间把当前列表定住，用户后面看到的就是他开始滚动时的那一份。
    if (!atTop) frozenRef.current = liveRef.current;
    setFollowing(atTop);
  }, []);

  const resume = useCallback(() => {
    followingRef.current = true;
    setFollowing(true);
    scrollRef.current?.scrollTo({ top: 0, behavior: "smooth" });
  }, []);

  useEffect(() => {
    if (!following) return;
    const ids = live.map((l) => l.id);

    // 首屏（或筛选后的第一份结果）不闪。
    if (!primedRef.current) {
      primedRef.current = true;
      seenRef.current = new Set(ids);
      return;
    }

    const fresh = ids.filter((id) => !seenRef.current.has(id));
    if (fresh.length === 0) return;

    if (seenRef.current.size > SEEN_CAP) seenRef.current = new Set();
    for (const id of ids) seenRef.current.add(id);

    setFlashIds(new Set(fresh));
    const timer = window.setTimeout(() => setFlashIds(NO_IDS), FLASH_MS);
    return () => window.clearTimeout(timer);
  }, [live, following]);

  const logs = following ? live : frozenRef.current;

  // 日志 id 是自增主键，新记录一定更大，所以「未读」= 比冻结快照里最大 id 还大的条数。
  let pending = 0;
  if (!following) {
    let maxFrozen = -1;
    for (const l of frozenRef.current) if (l.id > maxFrozen) maxFrozen = l.id;
    for (const l of live) if (l.id > maxFrozen) pending++;
  }

  return { scrollRef, onScroll, logs, flashIds, following, pending, resume };
}
