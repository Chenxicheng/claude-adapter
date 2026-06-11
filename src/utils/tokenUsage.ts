// Token usage storage utility
import { join } from 'path';
import { getTodayDateString, getBaseDir, enqueueJsonLineWrite } from './fileStorage';

export interface TokenUsageRecord {
  timestamp: string; // ISO 8601
  provider: string; // API endpoint/provider
  modelName: string; // Requested model name
  model?: string; // Actual model ID from API response
  inputTokens?: number;
  outputTokens?: number;
  cachedInputTokens?: number;
  streaming: boolean;
  usageStatus: 'complete' | 'missing_final_chunk';
}

const USAGE_DIR = join(getBaseDir(), 'token_usage');

/**
 * Get the file path for a given date
 */
function getUsageFilePath(dateStr: string): string {
  return join(USAGE_DIR, `${dateStr}.jsonl`);
}

/**
 * Record token usage to the daily file
 * Non-blocking, fails silently on errors
 */
export function recordUsage(data: Omit<TokenUsageRecord, 'timestamp'>): void {
  try {
    const record: TokenUsageRecord = {
      timestamp: new Date().toISOString(),
      ...data,
    };

    const filePath = getUsageFilePath(getTodayDateString());
    enqueueJsonLineWrite(USAGE_DIR, filePath, record);
  } catch {
    // Fail silently - don't interrupt API flow for usage tracking
  }
}
