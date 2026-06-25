import { TokenUsageRecord, UpstreamUsage } from '../utils/tokenUsage';
import { AnthropicUsage } from '../types/anthropic';
import { OpenAIUsage } from '../types/openai';

interface NormalizedUsage {
  inputTokens: number;
  outputTokens: number;
}

interface MessageDeltaUsage {
  input_tokens?: number;
  output_tokens: number;
}

export interface StreamUsageState {
  inputTokens: number;
  outputTokens: number;
  upstreamUsage?: UpstreamUsage;
  usageReceived: boolean;
}

export function isNonEmptyUsage(usage: unknown): usage is UpstreamUsage {
  return (
    typeof usage === 'object' &&
    usage !== null &&
    !Array.isArray(usage) &&
    Object.keys(usage).length > 0
  );
}

export function normalizeAnthropicUsageFromOpenAI(usage?: OpenAIUsage): NormalizedUsage {
  return {
    inputTokens: usage?.prompt_tokens ?? 0,
    outputTokens: usage?.completion_tokens ?? 0,
  };
}

export function buildAnthropicUsageFromOpenAI(usage?: OpenAIUsage): AnthropicUsage {
  const normalized = normalizeAnthropicUsageFromOpenAI(usage);

  return {
    input_tokens: normalized.inputTokens,
    output_tokens: normalized.outputTokens,
  };
}

export function applyOpenAIUsage(state: StreamUsageState, usage: OpenAIUsage): void {
  const normalized = normalizeAnthropicUsageFromOpenAI(usage);

  state.inputTokens = normalized.inputTokens;
  state.outputTokens = normalized.outputTokens;
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
  return usage;
}

export function buildStreamUsageRecord(args: {
  provider: string;
  modelName: string;
  model?: string;
  state: StreamUsageState;
}): Omit<TokenUsageRecord, 'timestamp' | 'schemaVersion'> {
  const base = {
    provider: args.provider,
    modelName: args.modelName,
    model: args.model,
    streaming: true,
    usageStatus: args.state.upstreamUsage ? 'complete' : 'missing_final_chunk',
  } as const;

  if (!args.state.upstreamUsage) {
    return base;
  }

  return {
    ...base,
    usage: args.state.upstreamUsage,
  };
}
