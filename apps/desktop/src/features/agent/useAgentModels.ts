/**
 * 模型选择：把「有哪些 provider/model 可选」和「当前选中哪一个」收在一处。
 *
 * 选择的持久化属于 workspace store，可用列表属于 Rust 的 provider 查询，
 * 而「列表刷新后当前选择已失效」这条回落规则属于两者之间 —— 它只在这里出现一次。
 */

import type { AiProviderConfig } from "@yukinal/shared";
import { useEffect } from "react";

import { useProviders } from "../../lib/providers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

/** 一个可选项 = 一对 (provider, model)，因为同名模型可以挂在多个 provider 下。 */
export type ModelChoice = {
  key: string;
  providerId: string;
  providerLabel: string;
  model: string;
  label: string;
};

export function modelChoiceKey(providerId: string, model: string): string {
  return `${providerId}:${model}`;
}

export function useAgentModels() {
  // 复用 `lib/providers.ts` 的规范查询，而不是在这里重写一遍 key 与 queryFn。
  // 两处写同一个 key 时，缓存是共享的，但 queryFn 是各写各的 —— 一旦 provider
  // 响应的取法变了（比如将来要多带一个字段），只会改到其中一处。
  const providers = useProviders();
  const selectedProviderId = useWorkspaceStore((state) => state.selectedProviderId);
  const selectedModel = useWorkspaceStore((state) => state.selectedModel);
  const selectProvider = useWorkspaceStore((state) => state.selectProvider);

  const modelChoices: ModelChoice[] = (providers.data ?? [])
    .filter((provider) => provider.enabled)
    .flatMap((provider) => {
      const models = provider.models?.length ? provider.models : [{ id: provider.model, label: provider.model }];
      return models.map((model) => ({
        key: modelChoiceKey(provider.id, model.id),
        providerId: provider.id,
        providerLabel: provider.label,
        model: model.id,
        label: model.label || model.id,
      }));
    });

  // 选中的 provider 可能被删除或被停用；落到第一个可用项，避免输入框假死。
  useEffect(() => {
    if (!providers.data?.length) return;
    const current = providers.data.find((provider: AiProviderConfig) => provider.id === selectedProviderId && provider.enabled);
    const fallback = providers.data.find((provider: AiProviderConfig) => provider.enabled);
    if (!current && fallback) selectProvider(fallback.id, fallback.model);
  }, [providers.data, selectedProviderId, selectProvider]);

  const selectedProvider = providers.data?.find(
    (provider: AiProviderConfig) => provider.id === selectedProviderId && provider.enabled,
  );
  const selectedModelKey = selectedProviderId && selectedModel ? modelChoiceKey(selectedProviderId, selectedModel) : null;
  const selectedChoice = modelChoices.find((choice) => choice.key === selectedModelKey);

  return {
    providers,
    modelChoices,
    selectedProvider,
    providerReady: Boolean(selectedProvider),
    selectedProviderId,
    selectedModel,
    selectedModelKey,
    /** 「provider · model」，用于让用户一眼看到这次运行会用哪个模型。 */
    selectedModelLabel: selectedChoice ? `${selectedChoice.providerLabel} · ${selectedChoice.label}` : null,
    /** 把渲染用的复合 key 还原成一次 provider 选择。 */
    selectModelKey: (key: string): void => {
      const [providerId, ...modelParts] = key.split(":");
      if (providerId && modelParts.length) selectProvider(providerId, modelParts.join(":"));
    },
  };
}
