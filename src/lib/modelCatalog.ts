/// Model metadata: context window + multimodal capability.
///
/// Primary source is models.dev (https://models.dev/api.json), a community
/// catalog keyed `{provider_id}.models.{model_id}` with `limit.context` and
/// `modalities.input`. Kernel aliases don't carry provider info and may be
/// redirect-prefixed ("openai/gpt-4o"), so lookup matches by bare model id
/// across every provider, exact-first then substring. Local regex presets
/// remain the fallback for anything the catalog doesn't know (custom or
/// renamed aliases), which is also what offline use gets.

import { defaultContextWindow, isVisionCapable } from "./modelMeta";

export type ModelMeta = {
  context: number;
  vision: boolean;
  source: "catalog" | "preset";
};

type CatalogModel = {
  limit?: { context?: number };
  modalities?: { input?: string[] };
};

export type Catalog = Map<string, ModelMeta>;

export async function fetchCatalog(): Promise<Catalog> {
  const resp = await fetch("https://models.dev/api.json");
  if (!resp.ok) throw new Error(`models.dev ${resp.status}`);
  const body: Record<string, { models?: Record<string, CatalogModel> }> =
    await resp.json();

  const cat: Catalog = new Map();
  for (const provider of Object.values(body)) {
    for (const [id, m] of Object.entries(provider.models ?? {})) {
      const ctx = m.limit?.context;
      if (typeof ctx !== "number" || ctx <= 0) continue;
      if (cat.has(id)) continue; // first provider wins; ambiguity is rare
      cat.set(id, {
        context: ctx,
        vision: (m.modalities?.input ?? []).includes("image"),
        source: "catalog",
      });
    }
  }
  return cat;
}

/// Strip redirect prefixes: "openai/gpt-4o" → "gpt-4o", keep the tail.
function bareName(alias: string): string {
  const i = alias.lastIndexOf("/");
  return i === -1 ? alias : alias.slice(i + 1);
}

export function lookupMeta(alias: string, cat: Catalog | null): ModelMeta {
  const name = bareName(alias);
  if (cat) {
    const exact = cat.get(name);
    if (exact) return exact;
    // Versioned variants ("kimi-k2-0905-preview") miss exact keys; a unique
    // substring match is still trustworthy, ambiguous ones are not.
    const partials = [...cat.keys()].filter((k) => name.includes(k));
    if (partials.length === 1) {
      return cat.get(partials[0])!;
    }
  }
  // Local fallback keeps vision detection from regex patterns too.
  return {
    context: defaultContextWindow(alias),
    vision: isVisionCapable(alias),
    source: "preset",
  };
}
