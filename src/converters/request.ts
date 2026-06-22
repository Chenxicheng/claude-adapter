// Request converter: Anthropic → OpenAI format
import {
  AnthropicMessageRequest,
  AnthropicMessage,
  AnthropicContentBlock,
  AnthropicToolUseBlock,
  AnthropicToolResultBlock,
  AnthropicSystemContent,
} from '../types/anthropic';
import {
  OpenAIChatRequest,
  OpenAIAssistantMessage,
  OpenAIMessage,
  OpenAIToolCall,
  OpenAIUserContentPart,
  OpenAIToolMessage,
} from '../types/openai';
import { convertToolsToOpenAI, convertToolChoiceToOpenAI } from './tools';
import { getCachedUpdateInfo } from '../utils/update';
import {
  isGlm52Model,
  isGlm5Model,
  isOpenAIGpt5Model,
  isOpenAIOSeriesModel,
  isOpenAIReasoningModel,
  isQwen3Model,
} from '../utils/modelFamily';
import { version } from '../../package.json';

// Claude Code system prompt identifier to detect and rebrand
const CLAUDE_CODE_IDENTIFIER = "You are Claude Code, Anthropic's official CLI for Claude.";
const ANTHROPIC_BILLING_HEADER_PREFIX = 'x-anthropic-billing-header:';
const OPENAI_REASONING_BUDGET = {
  low: 4000,
  high: 16000,
};
const ASSISTANT_PREFILL_TOKENS = new Set(['{', '[', '```', '{"', '[{']);
const TOOL_ID_CHARS = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';

/**
 * Modify system prompt to replace Claude Code identifier with Claude Adapter branding
 * Includes update notification when a new version is available
 */
function modifySystemPromptForClaudeAdapter(systemContent: string): string {
  if (systemContent.includes(CLAUDE_CODE_IDENTIFIER)) {
    // Check for available updates from cache
    const updateInfo = getCachedUpdateInfo();

    let replacement = `You are Claude Code, running on Claude Adapter V${version} (https://github.com/shantoislamdev/claude-adapter). Learn more about Claude Adapter at https://claude-adapter.pages.dev/. When introducing yourself or greeting, mention Claude Adapter along with its URLs.`;

    // Add update notification if new version available
    if (updateInfo?.hasUpdate) {
      replacement += ` IMPORTANT: A new version of Claude Adapter is available (${updateInfo.current} → ${updateInfo.latest}). Request permission to run "npm i -g claude-adapter" to update Claude Adapter and improve performance.`;
    }

    return systemContent.replace(CLAUDE_CODE_IDENTIFIER, replacement);
  }
  return systemContent;
}

function stripLeadingAnthropicBillingHeader(systemContent: string): string {
  if (!systemContent.startsWith(ANTHROPIC_BILLING_HEADER_PREFIX)) {
    return systemContent;
  }

  const lineEnd = systemContent.search(/\r?\n/);
  if (lineEnd === -1) {
    return '';
  }

  return systemContent.slice(lineEnd).replace(/^\r?\n/, '');
}

function createStatusError(message: string, status: number): Error & { status: number } {
  const error = new Error(message) as Error & { status: number };
  error.status = status;
  return error;
}

function hasEnabledThinking(anthropicRequest: AnthropicMessageRequest): boolean {
  return (
    anthropicRequest.thinking?.type === 'enabled' || anthropicRequest.thinking?.type === 'adaptive'
  );
}

function resolveOpenAIReasoningEffort(
  anthropicRequest: AnthropicMessageRequest
): OpenAIChatRequest['reasoning_effort'] | undefined {
  const explicitEffort = anthropicRequest.output_config?.effort;
  if (explicitEffort) {
    switch (explicitEffort) {
      case 'low':
        return 'low';
      case 'medium':
        return 'medium';
      case 'high':
        return 'high';
      case 'max':
        return 'xhigh';
      default:
        return undefined;
    }
  }

  const thinkingType = anthropicRequest.thinking?.type;
  if (thinkingType === 'adaptive') {
    return 'xhigh';
  }
  if (thinkingType !== 'enabled') {
    return undefined;
  }

  const budget = anthropicRequest.thinking?.budget_tokens;
  if (budget === undefined) {
    return 'high';
  }
  if (budget < OPENAI_REASONING_BUDGET.low) {
    return 'low';
  }
  if (budget < OPENAI_REASONING_BUDGET.high) {
    return 'medium';
  }
  return 'high';
}

function resolveGlmReasoningEffort(
  anthropicRequest: AnthropicMessageRequest
): OpenAIChatRequest['reasoning_effort'] | undefined {
  const explicitEffort = anthropicRequest.output_config?.effort;
  if (explicitEffort) {
    switch (explicitEffort) {
      case 'max':
        return 'max';
      case 'high':
        return 'high';
      case 'medium':
      case 'low':
        return 'high';
      default:
        return undefined;
    }
  }

  if (anthropicRequest.thinking?.type === 'adaptive') {
    return 'max';
  }

  return undefined;
}

function shouldUseMaxCompletionTokens(targetModel: string): boolean {
  return isOpenAIOSeriesModel(targetModel) || isOpenAIGpt5Model(targetModel);
}

function applyOpenAIRequestOptions(
  openaiRequest: OpenAIChatRequest,
  anthropicRequest: AnthropicMessageRequest,
  targetModel: string
): void {
  if (!isOpenAIReasoningModel(targetModel)) {
    return;
  }

  const reasoningEffort = resolveOpenAIReasoningEffort(anthropicRequest);
  if (reasoningEffort) {
    openaiRequest.reasoning_effort = reasoningEffort;
  }
}

function applyGlmRequestOptions(
  openaiRequest: OpenAIChatRequest,
  anthropicRequest: AnthropicMessageRequest,
  targetModel: string
): void {
  if (!isGlm5Model(targetModel)) {
    return;
  }

  if (anthropicRequest.thinking?.type) {
    openaiRequest.thinking = {
      type: anthropicRequest.thinking.type === 'disabled' ? 'disabled' : 'enabled',
    };
  }

  if (anthropicRequest.stream && anthropicRequest.tools && anthropicRequest.tools.length > 0) {
    openaiRequest.tool_stream = true;
  }

  if (!isGlm52Model(targetModel) || openaiRequest.thinking?.type !== 'enabled') {
    return;
  }

  const reasoningEffort = resolveGlmReasoningEffort(anthropicRequest);
  if (reasoningEffort) {
    openaiRequest.reasoning_effort = reasoningEffort;
  }
}

function applyQwenRequestOptions(
  openaiRequest: OpenAIChatRequest,
  anthropicRequest: AnthropicMessageRequest,
  targetModel: string
): void {
  if (!isQwen3Model(targetModel) || !hasEnabledThinking(anthropicRequest)) {
    return;
  }

  if (anthropicRequest.stream !== true) {
    throw createStatusError(
      'Qwen thinking mode in this adapter requires stream=true because the upstream provider only supports it reliably on streaming calls.',
      400
    );
  }

  openaiRequest.enable_thinking = true;
}

function applyModelFamilyRequestOptions(
  openaiRequest: OpenAIChatRequest,
  anthropicRequest: AnthropicMessageRequest,
  targetModel: string
): void {
  applyOpenAIRequestOptions(openaiRequest, anthropicRequest, targetModel);
  applyGlmRequestOptions(openaiRequest, anthropicRequest, targetModel);
  applyQwenRequestOptions(openaiRequest, anthropicRequest, targetModel);
}

function getSystemContent(system: NonNullable<AnthropicMessageRequest['system']>): string {
  if (typeof system === 'string') {
    return system;
  }

  return system.map((s: AnthropicSystemContent) => s.text).join('\n');
}

function getMaxTokens(
  anthropicRequest: AnthropicMessageRequest,
  isAzureEndpoint: boolean
): number {
  if (isAzureEndpoint && anthropicRequest.max_tokens === 1) {
    return 32;
  }

  return anthropicRequest.max_tokens;
}

/**
 * Convert Anthropic Messages API request to OpenAI Chat Completions format
 */
export function convertRequestToOpenAI(
  anthropicRequest: AnthropicMessageRequest,
  targetModel: string,
  isAzureEndpoint: boolean = false
): OpenAIChatRequest {
  const messages: OpenAIMessage[] = [];
  const preserveReasoningContent = shouldPreserveReasoningContent(targetModel);

  // Handle system prompt - becomes first message with role: system
  if (anthropicRequest.system) {
    const systemContent = getSystemContent(anthropicRequest.system);
    const strippedSystemContent = stripLeadingAnthropicBillingHeader(systemContent);
    const modifiedSystemContent = modifySystemPromptForClaudeAdapter(strippedSystemContent);

    if (modifiedSystemContent.length > 0) {
      messages.push({
        role: 'system',
        content: modifiedSystemContent,
      });
    }
  }

  // Track tool ID deduplication across messages
  // Maps original ID -> array of unique IDs (for handling duplicates)
  const idDeduplication = {
    seenIds: new Set<string>(),
    idMappings: new Map<string, string[]>(),
    resultIndex: new Map<string, number>(), // Tracks which mapping to use for tool_results
  };

  // Convert messages with shared deduplication context
  for (const msg of anthropicRequest.messages) {
    const converted = convertMessage(msg, idDeduplication, preserveReasoningContent);
    messages.push(...converted);
  }

  // Ensure at least one message survived conversion. If all input messages had
  // missing content (e.g., only hook injections), the resulting array would be
  // empty and the upstream provider would reject it with a cryptic error.
  if (messages.length === 0) {
    throw new Error('No messages after conversion: all input messages had missing content');
  }

  // Azure OpenAI enforces strict validation on max_tokens.
  // Claude Code uses max_tokens: 1 for prompt caching optimization,
  // but this causes 400 errors with Azure OpenAI. Convert to 32 to allow
  // at least a brief acknowledgment or the start of a tool call.
  const maxTokens = getMaxTokens(anthropicRequest, isAzureEndpoint);

  const openaiRequest: OpenAIChatRequest = {
    model: targetModel,
    messages,
    stream: anthropicRequest.stream,
  };

  if (shouldUseMaxCompletionTokens(targetModel)) {
    openaiRequest.max_completion_tokens = maxTokens;
  } else {
    openaiRequest.max_tokens = maxTokens;
  }

  // specific handling for streaming requests to include usage data
  if (anthropicRequest.stream) {
    openaiRequest.stream_options = { include_usage: true };
  }

  // Optional parameters
  if (anthropicRequest.temperature !== undefined) {
    openaiRequest.temperature = anthropicRequest.temperature;
  }

  if (anthropicRequest.top_p !== undefined) {
    openaiRequest.top_p = anthropicRequest.top_p;
  }
  if (anthropicRequest.stop_sequences) {
    openaiRequest.stop = anthropicRequest.stop_sequences;
  }
  // Note: metadata.user_id is intentionally NOT mapped to OpenAI's 'user' field
  // because some providers (e.g., Mistral) strictly reject unsupported parameters

  if (anthropicRequest.tools && anthropicRequest.tools.length > 0) {
    openaiRequest.tools = convertToolsToOpenAI(anthropicRequest.tools);
  }
  if (anthropicRequest.tool_choice) {
    openaiRequest.tool_choice = convertToolChoiceToOpenAI(anthropicRequest.tool_choice);
  }

  applyModelFamilyRequestOptions(openaiRequest, anthropicRequest, targetModel);

  return openaiRequest;
}

function shouldPreserveReasoningContent(targetModel: string): boolean {
  return isGlm5Model(targetModel) || isQwen3Model(targetModel);
}

/**
 * Check if content is an assistant prefill token (JSON starter)
 * Anthropic supports prefilling assistant responses, but other providers don't
 */
function isAssistantPrefill(content: string): boolean {
  const trimmed = content.trim();
  return ASSISTANT_PREFILL_TOKENS.has(trimmed) || trimmed.length <= 2;
}

/**
 * Context for tracking tool ID deduplication across messages
 */
interface IdDeduplicationContext {
  seenIds: Set<string>;
  idMappings: Map<string, string[]>;
  resultIndex: Map<string, number>;
}

/**
 * Convert a single Anthropic message to OpenAI format
 * May return multiple messages (e.g., tool results become separate messages)
 */
function convertMessage(
  msg: AnthropicMessage,
  ctx: IdDeduplicationContext,
  preserveReasoningContent: boolean
): OpenAIMessage[] {
  const result: OpenAIMessage[] = [];

  // Skip messages with missing content.
  // Some Claude Code message types (e.g., session-start hook attachments) may
  // have a valid role but no content.
  if (msg.content === undefined || msg.content === null) {
    return result;
  }

  if (typeof msg.content === 'string') {
    // Simple string content
    if (msg.role === 'user') {
      result.push({ role: 'user', content: msg.content });
    } else {
      // Skip assistant prefill messages (e.g., "{" for JSON output).
      // These are Anthropic-specific and cause 400 errors with other providers.
      // Note: unknown roles also fall into this branch and are treated as assistant
      // for forward compatibility when Claude Code introduces new message types.
      if (isAssistantPrefill(msg.content)) {
        return result; // Return empty - skip this message
      }
      result.push({ role: 'assistant', content: msg.content });
    }
  } else {
    // Array of content blocks
    if (msg.role === 'user') {
      const { userContent, toolResults } = processUserContentBlocks(msg.content, ctx);

      // Add tool results as separate tool messages
      result.push(...toolResults);

      // Add user content if any
      if (userContent.length > 0) {
        result.push({
          role: 'user',
          content:
            userContent.length === 1 && userContent[0].type === 'text'
              ? userContent[0].text
              : userContent,
        });
      }
    } else {
      const assistantMsg = convertAssistantContentBlocks(
        msg.content,
        ctx,
        preserveReasoningContent
      );
      if (assistantMsg) {
        result.push(assistantMsg);
      }
    }
  }

  return result;
}

function convertAssistantContentBlocks(
  blocks: AnthropicContentBlock[],
  ctx: IdDeduplicationContext,
  preserveReasoningContent: boolean
): OpenAIAssistantMessage | undefined {
  const { textContent, toolCalls, reasoningContent } = processAssistantContentBlocks(blocks, ctx);

  if (toolCalls.length === 0 && textContent && isAssistantPrefill(textContent)) {
    return undefined;
  }

  const assistantMsg: OpenAIAssistantMessage = {
    role: 'assistant',
    content: textContent || null,
  };

  if (toolCalls.length > 0) {
    assistantMsg.tool_calls = toolCalls;
  }
  if (preserveReasoningContent && toolCalls.length > 0 && reasoningContent.length > 0) {
    assistantMsg.reasoning_content = reasoningContent;
  }

  return assistantMsg;
}

/**
 * Process user content blocks, separating tool results from regular content
 */
function processUserContentBlocks(
  blocks: AnthropicContentBlock[],
  ctx: IdDeduplicationContext
): {
  userContent: OpenAIUserContentPart[];
  toolResults: OpenAIToolMessage[];
} {
  const userContent: OpenAIUserContentPart[] = [];
  const toolResults: OpenAIToolMessage[] = [];

  for (const block of blocks) {
    if (block.type === 'text') {
      userContent.push({ type: 'text', text: block.text });
    } else if (block.type === 'tool_result') {
      const content = extractToolResultContent(block);
      const toolCallId = resolveToolResultCallId(block.tool_use_id, ctx);

      toolResults.push({
        role: 'tool',
        tool_call_id: toolCallId,
        content: block.is_error ? `Error: ${content}` : content,
      });
    }
    // Images would need special handling for vision models - not implemented here
  }

  return { userContent, toolResults };
}

function extractToolResultContent(toolResult: AnthropicToolResultBlock): string {
  if (typeof toolResult.content === 'string') {
    return toolResult.content;
  }

  if (!Array.isArray(toolResult.content)) {
    return '';
  }

  return toolResult.content
    .filter(
      (contentBlock): contentBlock is { type: 'text'; text: string } =>
        contentBlock.type === 'text'
    )
    .map((contentBlock) => contentBlock.text)
    .join('\n');
}

function resolveToolResultCallId(toolUseId: string, ctx: IdDeduplicationContext): string {
  const mappings = ctx.idMappings.get(toolUseId);
  if (!mappings) {
    return toolUseId;
  }

  const idx = ctx.resultIndex.get(toolUseId) || 0;
  if (idx >= mappings.length) {
    return toolUseId;
  }

  ctx.resultIndex.set(toolUseId, idx + 1);
  return mappings[idx];
}

/**
 * Process assistant content blocks, extracting text and tool calls
 * Deduplicates tool IDs to prevent errors with providers that reject duplicates
 */
function processAssistantContentBlocks(
  blocks: AnthropicContentBlock[],
  ctx: IdDeduplicationContext
): {
  textContent: string;
  reasoningContent: string;
  toolCalls: OpenAIToolCall[];
} {
  let textContent = '';
  let reasoningContent = '';
  const toolCalls: OpenAIToolCall[] = [];

  for (const block of blocks) {
    if (block.type === 'text') {
      textContent += block.text;
    } else if (block.type === 'thinking') {
      reasoningContent += block.thinking;
    } else if (block.type === 'tool_use') {
      const toolUse = block;
      const idToUse = getUniqueToolUseId(toolUse, ctx);
      ctx.seenIds.add(idToUse);

      // Track the mapping for tool_result matching
      if (!ctx.idMappings.has(toolUse.id)) {
        ctx.idMappings.set(toolUse.id, []);
      }
      ctx.idMappings.get(toolUse.id)!.push(idToUse);

      toolCalls.push({
        id: idToUse,
        type: 'function',
        function: {
          name: toolUse.name,
          arguments: JSON.stringify(toolUse.input),
        },
      });
    }
  }

  return { textContent, reasoningContent, toolCalls };
}

function getUniqueToolUseId(
  toolUse: AnthropicToolUseBlock,
  ctx: IdDeduplicationContext
): string {
  if (!ctx.seenIds.has(toolUse.id)) {
    return toolUse.id;
  }

  const repairedId = repairDuplicateToolId(toolUse.id);
  console.log(`[adapter] Repair ID: ${toolUse.id} → ${repairedId}`);
  return repairedId;
}

function repairDuplicateToolId(originalId: string): string {
  if (originalId.length > 11) {
    return `${originalId.substring(0, 8)}${randomToolIdSuffix(originalId.length - 8)}`;
  }

  return randomToolIdSuffix(originalId.length);
}

function randomToolIdSuffix(length: number): string {
  let suffix = '';
  for (let i = 0; i < length; i++) {
    suffix += TOOL_ID_CHARS.charAt(Math.floor(Math.random() * TOOL_ID_CHARS.length));
  }
  return suffix;
}
