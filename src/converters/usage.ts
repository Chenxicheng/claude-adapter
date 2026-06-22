import { TokenUsageRecord } from '../utils/tokenUsage';
import { AnthropicUsage } from '../types/anthropic';
import { OpenAIUsage } from '../types/openai';

export interface StreamUsageState {
  inputTokens: number;
  outputTokens: number;
  cachedInputTokens?: number;
  cacheCreationInputTokens?: number;
  usageReceived: boolean;
}

export function getCacheReadInputTokens(usage?: OpenAIUsage): number | undefined {
  if (!usage) {
    return undefined;
  }

  if (usage.cache_read_input_tokens !== undefined) {
    return usage.cache_read_input_tokens;
  }

  return usage.prompt_tokens_details?.cached_tokens;
}

export function normalizeAnthropicUsageFromOpenAI(usage?: OpenAIUsage): {
  inputTokens: number;
  outputTokens: number;
  cacheReadInputTokens?: number;
  cacheCreationInputTokens?: number;
} {
  const promptTokens = usage?.prompt_tokens ?? 0;
  const outputTokens = usage?.completion_tokens ?? 0;
  const cacheReadInputTokens = getCacheReadInputTokens(usage);
  const cacheCreationInputTokens = usage?.cache_creation_input_tokens;
  const inputTokens = Math.max(
    0,
    promptTokens - (cacheReadInputTokens ?? 0) - (cacheCreationInputTokens ?? 0)
  );

  return {
    inputTokens,
    outputTokens,
    ...(cacheReadInputTokens !== undefined ? { cacheReadInputTokens } : {}),
    ...(cacheCreationInputTokens !== undefined ? { cacheCreationInputTokens } : {}),
  };
}

export function buildAnthropicUsageFromOpenAI(usage?: OpenAIUsage): AnthropicUsage {
  const normalized = normalizeAnthropicUsageFromOpenAI(usage);

  return {
    input_tokens: normalized.inputTokens,
    output_tokens: normalized.outputTokens,
    ...(normalized.cacheReadInputTokens !== undefined
      ? { cache_read_input_tokens: normalized.cacheReadInputTokens }
      : {}),
    ...(normalized.cacheCreationInputTokens !== undefined
      ? { cache_creation_input_tokens: normalized.cacheCreationInputTokens }
      : {}),
  };
}

export function applyOpenAIUsage(state: StreamUsageState, usage: OpenAIUsage): void {
  const normalized = normalizeAnthropicUsageFromOpenAI(usage);

  state.inputTokens = normalized.inputTokens;
  state.outputTokens = normalized.outputTokens;
  state.cachedInputTokens = normalized.cacheReadInputTokens;
  state.cacheCreationInputTokens = normalized.cacheCreationInputTokens;
  state.usageReceived = true;
}

export function buildMessageStartUsage(): {
  input_tokens: number;
  output_tokens: number;
} {
  return {
    input_tokens: 0,
    output_tokens: 0,
  };
}

export function buildMessageDeltaUsage(state: StreamUsageState): {
  input_tokens?: number;
  output_tokens: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
} {
  return {
    output_tokens: state.outputTokens,
    ...(state.usageReceived ? { input_tokens: state.inputTokens } : {}),
    ...(state.cachedInputTokens !== undefined
      ? { cache_read_input_tokens: state.cachedInputTokens }
      : {}),
    ...(state.cacheCreationInputTokens !== undefined
      ? { cache_creation_input_tokens: state.cacheCreationInputTokens }
      : {}),
  };
}

export function buildStreamUsageRecord(args: {
  provider: string;
  modelName: string;
  model?: string;
  state: StreamUsageState;
}): Omit<TokenUsageRecord, 'timestamp'> {
  const base = {
    provider: args.provider,
    modelName: args.modelName,
    model: args.model,
    streaming: true,
    usageStatus: args.state.usageReceived ? 'complete' : 'missing_final_chunk',
  } as const;

  if (!args.state.usageReceived) {
    return base;
  }

  return {
    ...base,
    inputTokens: args.state.inputTokens,
    outputTokens: args.state.outputTokens,
    ...(args.state.cachedInputTokens !== undefined
      ? { cachedInputTokens: args.state.cachedInputTokens }
      : {}),
    ...(args.state.cacheCreationInputTokens !== undefined
      ? { cacheCreationInputTokens: args.state.cacheCreationInputTokens }
      : {}),
  };
}
