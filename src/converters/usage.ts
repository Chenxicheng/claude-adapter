import { TokenUsageRecord } from '../utils/tokenUsage';
import { AnthropicUsage } from '../types/anthropic';
import { OpenAIUsage } from '../types/openai';

interface NormalizedUsage {
  inputTokens: number;
  outputTokens: number;
  cacheReadInputTokens?: number;
}

interface MessageDeltaUsage {
  input_tokens?: number;
  output_tokens: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
}

export interface StreamUsageState {
  inputTokens: number;
  outputTokens: number;
  cachedInputTokens?: number;
  usageReceived: boolean;
}

export function getCacheReadInputTokens(usage?: OpenAIUsage): number | undefined {
  if (!usage) {
    return undefined;
  }

  return usage.prompt_tokens_details?.cached_tokens;
}

export function normalizeAnthropicUsageFromOpenAI(usage?: OpenAIUsage): NormalizedUsage {
  const promptTokens = usage?.prompt_tokens ?? 0;
  const outputTokens = usage?.completion_tokens ?? 0;
  const cacheReadInputTokens = getCacheReadInputTokens(usage);
  const inputTokens = Math.max(0, promptTokens - (cacheReadInputTokens ?? 0));

  const normalized: NormalizedUsage = {
    inputTokens,
    outputTokens,
  };
  applyNormalizedCacheFields(normalized, cacheReadInputTokens);
  return normalized;
}

export function buildAnthropicUsageFromOpenAI(usage?: OpenAIUsage): AnthropicUsage {
  const normalized = normalizeAnthropicUsageFromOpenAI(usage);

  const anthropicUsage: AnthropicUsage = {
    input_tokens: normalized.inputTokens,
    output_tokens: normalized.outputTokens,
  };
  applyAnthropicCacheFields(anthropicUsage, normalized);
  return anthropicUsage;
}

export function applyOpenAIUsage(state: StreamUsageState, usage: OpenAIUsage): void {
  const normalized = normalizeAnthropicUsageFromOpenAI(usage);

  state.inputTokens = normalized.inputTokens;
  state.outputTokens = normalized.outputTokens;
  state.cachedInputTokens = normalized.cacheReadInputTokens;
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

export function buildMessageDeltaUsage(state: StreamUsageState): MessageDeltaUsage {
  const usage: MessageDeltaUsage = {
    output_tokens: state.outputTokens,
  };
  if (state.usageReceived) {
    usage.input_tokens = state.inputTokens;
  }
  applyAnthropicCacheFields(usage, {
    inputTokens: state.inputTokens,
    outputTokens: state.outputTokens,
    cacheReadInputTokens: state.cachedInputTokens,
  });
  return usage;
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

  const record: Omit<TokenUsageRecord, 'timestamp'> = {
    ...base,
    inputTokens: args.state.inputTokens,
    outputTokens: args.state.outputTokens,
  };
  applyRecordCacheFields(record, args.state);
  return record;
}

function applyNormalizedCacheFields(
  usage: NormalizedUsage,
  cacheReadInputTokens: number | undefined
): void {
  if (cacheReadInputTokens !== undefined) {
    usage.cacheReadInputTokens = cacheReadInputTokens;
  }
}

function applyAnthropicCacheFields(
  usage: AnthropicUsage | MessageDeltaUsage,
  normalized: NormalizedUsage
): void {
  if (normalized.cacheReadInputTokens !== undefined) {
    usage.cache_read_input_tokens = normalized.cacheReadInputTokens;
  }
}

function applyRecordCacheFields(
  record: Omit<TokenUsageRecord, 'timestamp'>,
  state: StreamUsageState
): void {
  if (state.cachedInputTokens !== undefined) {
    record.cachedInputTokens = state.cachedInputTokens;
  }
}
