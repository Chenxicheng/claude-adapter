import { TokenUsageRecord } from '../utils/tokenUsage';

export interface StreamUsageState {
  inputTokens: number;
  outputTokens: number;
  cachedInputTokens?: number;
  usageReceived: boolean;
}

export function applyOpenAIUsage(
  state: StreamUsageState,
  usage: {
    prompt_tokens: number;
    completion_tokens: number;
    prompt_tokens_details?: { cached_tokens?: number };
  }
): void {
  state.inputTokens = usage.prompt_tokens;
  state.outputTokens = usage.completion_tokens;
  state.cachedInputTokens = usage.prompt_tokens_details?.cached_tokens;
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
} {
  return {
    output_tokens: state.outputTokens,
    ...(state.usageReceived ? { input_tokens: state.inputTokens } : {}),
    ...(state.cachedInputTokens !== undefined
      ? { cache_read_input_tokens: state.cachedInputTokens }
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
  };
}
