// Configuration types for claude-adapter

interface AdapterConfigBase {
    baseUrl: string;
    models: ModelConfig;
    upstreamHeaders?: Record<string, string>;
    port?: number;
}

export type AdapterConfig = AdapterConfigBase & (
    | { apiKey: string; apiKeyEnv?: never }
    | { apiKey?: never; apiKeyEnv: string }
);

export interface ModelConfig {
    opus: string;
    sonnet: string;
    haiku: string;
}

export interface ClaudeSettings {
    env?: Record<string, string>;
    [key: string]: unknown; // Preserve other settings
}

export interface ClaudeJson {
    hasCompletedOnboarding?: boolean;
    [key: string]: unknown; // Preserve other settings
}
