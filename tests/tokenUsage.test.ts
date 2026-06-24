// Tests for token usage utilities
import { existsSync, mkdirSync, rmSync, readFileSync } from 'fs';
import { join } from 'path';
import { tmpdir } from 'os';

const TEST_DIR = join(tmpdir(), 'claude-adapter-tokenusage-test-' + Date.now());

// Mock fileStorage to use test directory
jest.mock('../src/utils/fileStorage', () => {
  const actual = jest.requireActual('../src/utils/fileStorage');
  return {
    ...actual,
    getBaseDir: () => TEST_DIR,
  };
});

import { recordUsage, TokenUsageRecord } from '../src/utils/tokenUsage';
import { flushJsonLineWrites } from '../src/utils/fileStorage';

function readLastUsageRecord(): TokenUsageRecord {
  const usageDir = join(TEST_DIR, 'token_usage');
  const files = require('fs').readdirSync(usageDir);
  const content = readFileSync(join(usageDir, files[0]), 'utf-8').trim();
  const lastLine = content.split('\n').at(-1)!;
  return JSON.parse(lastLine);
}

describe('Token Usage Utilities', () => {
  beforeAll(() => {
    if (!existsSync(TEST_DIR)) {
      mkdirSync(TEST_DIR, { recursive: true });
    }
  });

  afterAll(async () => {
    await flushJsonLineWrites();

    try {
      rmSync(TEST_DIR, { recursive: true, force: true });
    } catch {
      // Ignore cleanup errors
    }
  });

  describe('recordUsage', () => {
    it('should record token usage to file', async () => {
      const usage = {
        provider: 'https://api.openai.com/v1',
        modelName: 'claude-4-opus',
        model: 'gpt-4-turbo',
        usage: {
          prompt_tokens: 100,
          completion_tokens: 50,
          total_tokens: 150,
        },
        streaming: false,
        usageStatus: 'complete' as const,
      };

      recordUsage(usage);
      await flushJsonLineWrites();

      const usageDir = join(TEST_DIR, 'token_usage');
      expect(existsSync(usageDir)).toBe(true);
    });

    it('should include timestamp in record', async () => {
      const usage = {
        provider: 'https://api.example.com',
        modelName: 'test-model',
        usage: {
          prompt_tokens: 200,
          completion_tokens: 100,
        },
        streaming: true,
        usageStatus: 'complete' as const,
      };

      recordUsage(usage);
      await flushJsonLineWrites();

      const usageDir = join(TEST_DIR, 'token_usage');
      const files = require('fs').readdirSync(usageDir);
      expect(files.length).toBeGreaterThan(0);

      const content = readFileSync(join(usageDir, files[0]), 'utf-8');
      expect(content).toContain('timestamp');
      expect(content).toContain('test-model');
    });

    it('should persist schema version in every record', async () => {
      const usage = {
        provider: 'https://api.example.com',
        modelName: 'versioned-model',
        usage: {
          prompt_tokens: 300,
          completion_tokens: 150,
        },
        streaming: false,
        usageStatus: 'complete' as const,
      };

      recordUsage(usage);
      await flushJsonLineWrites();

      const record = readLastUsageRecord();
      expect(record.schemaVersion).toBe(2);
    });

    it('should record raw upstream usage', async () => {
      const usage = {
        provider: 'https://api.example.com',
        modelName: 'raw-usage-model',
        usage: {
          prompt_tokens: 200,
          completion_tokens: 100,
          total_tokens: 300,
          prompt_tokens_details: {
            cached_tokens: 125,
          },
        },
        streaming: false,
        usageStatus: 'complete' as const,
      };

      recordUsage(usage);
      await flushJsonLineWrites();

      const record = readLastUsageRecord();
      expect(record.usage).toEqual(usage.usage);
      expect(record).not.toHaveProperty('inputTokens');
      expect(record).not.toHaveProperty('outputTokens');
      expect(record).not.toHaveProperty('cachedInputTokens');
      expect(record).not.toHaveProperty('cacheCreationInputTokens');
    });

    it('should preserve unknown upstream usage fields', async () => {
      const usage = {
        provider: 'https://api.example.com',
        modelName: 'unknown-usage-model',
        usage: {
          billable_units: 42,
          provider_specific: {
            cache_tier: 'warm',
          },
        },
        streaming: false,
        usageStatus: 'complete' as const,
      };

      recordUsage(usage);
      await flushJsonLineWrites();

      const record = readLastUsageRecord();
      expect(record.usage).toEqual(usage.usage);
    });

    it('should handle optional model field', async () => {
      const usage = {
        provider: 'https://api.example.com',
        modelName: 'requested-model',
        model: 'actual-model-id',
        usage: {
          prompt_tokens: 400,
          completion_tokens: 200,
        },
        streaming: true,
        usageStatus: 'complete' as const,
      };

      recordUsage(usage);
      await flushJsonLineWrites();

      const usageDir = join(TEST_DIR, 'token_usage');
      const files = require('fs').readdirSync(usageDir);
      const content = readFileSync(join(usageDir, files[0]), 'utf-8');

      expect(content).toContain('actual-model-id');
    });

    it('should append multiple records', async () => {
      // Clear previous records by using unique identifiers
      const usage1 = {
        provider: 'provider-1',
        modelName: 'model-1',
        usage: {
          prompt_tokens: 10,
          completion_tokens: 5,
        },
        streaming: false,
        usageStatus: 'complete' as const,
      };
      const usage2 = {
        provider: 'provider-2',
        modelName: 'model-2',
        usage: {
          prompt_tokens: 20,
          completion_tokens: 10,
        },
        streaming: true,
        usageStatus: 'complete' as const,
      };

      recordUsage(usage1);
      recordUsage(usage2);
      await flushJsonLineWrites();

      const usageDir = join(TEST_DIR, 'token_usage');
      const files = require('fs').readdirSync(usageDir);
      const content = readFileSync(join(usageDir, files[0]), 'utf-8');

      expect(content).toContain('model-1');
      expect(content).toContain('model-2');
    });

    it('should write queued records as valid JSON lines', async () => {
      const modelNames = ['queued-model-1', 'queued-model-2', 'queued-model-3'];

      for (const modelName of modelNames) {
        recordUsage({
          provider: 'queued-provider',
          modelName,
          usage: {
            prompt_tokens: 1,
            completion_tokens: 1,
          },
          streaming: false,
          usageStatus: 'complete',
        });
      }

      await flushJsonLineWrites();

      const usageDir = join(TEST_DIR, 'token_usage');
      const files = require('fs').readdirSync(usageDir);
      const content = readFileSync(join(usageDir, files[0]), 'utf-8');
      const queuedRecords = content
        .trim()
        .split('\n')
        .map((line) => JSON.parse(line) as TokenUsageRecord)
        .filter((record) => modelNames.includes(record.modelName));

      expect(queuedRecords.map((record) => record.modelName)).toEqual(modelNames);
    });

    it('should persist usage status for complete records', async () => {
      recordUsage({
        provider: 'https://api.example.com',
        modelName: 'complete-model',
        usage: {
          prompt_tokens: 8,
          completion_tokens: 4,
        },
        streaming: true,
        usageStatus: 'complete',
      });

      await flushJsonLineWrites();
      const record = readLastUsageRecord();

      expect(record.usageStatus).toBe('complete');
      expect(record.usage).toEqual({
        prompt_tokens: 8,
        completion_tokens: 4,
      });
    });

    it('should allow missing final chunk records without token counts', async () => {
      recordUsage({
        provider: 'https://api.example.com',
        modelName: 'missing-model',
        streaming: true,
        usageStatus: 'missing_final_chunk',
      });

      await flushJsonLineWrites();
      const record = readLastUsageRecord();

      expect(record.usageStatus).toBe('missing_final_chunk');
      expect(record.schemaVersion).toBe(2);
      expect(record).not.toHaveProperty('usage');
    });
  });
});
