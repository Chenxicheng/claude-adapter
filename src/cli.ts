#!/usr/bin/env node
// CLI entry point for claude-adapter
import { Command } from 'commander';
import inquirer from 'inquirer';
import { join } from 'node:path';
import { AdapterConfig, AssistantPrefillMode } from './types/config';
import {
  getConfigDir,
  loadConfig,
  saveConfig,
  updateClaudeJson,
  updateClaudeSettings,
} from './utils/config';
import { startNativeServer } from './native';
import { UI } from './utils/ui';
import { checkForUpdates } from './utils/update';
import { getMetadata } from './utils/metadata';
import { parseUpstreamHeadersInput } from './utils/upstreamHeaders';
import { parseApiKeyEnvInput } from './utils/apiKey';
import { version } from '../package.json';

const program = new Command();

program
  .name('claude-adapter')
  .description('Proxy adapter to use OpenAI API with Claude Code')
  .version(version);

program
  .option('-p, --port <port>', 'Port to run the proxy server on', '3080')
  .option('-r, --reconfigure', 'Force reconfiguration even if config exists')
  .option('--no-claude-settings', 'Skip updating Claude Code settings files')
  .action(async (options) => {
    UI.banner();
    UI.header('Adapt any model for Claude Code');

    try {
      // Initialize metadata (creates metadata.json on first run)
      getMetadata();

      // Step 1: Update ~/.claude.json for onboarding skip (if enabled)
      if (options.claudeSettings) {
        updateClaudeJson();
        UI.statusDone(true, 'Initialized Claude Adapter');
      } else {
        UI.info('Skipping Claude settings update (--no-claude-settings)');
      }

      // Step 2: Load or create configuration
      let config = loadConfig();

      if (!config || options.reconfigure) {
        UI.log(''); // Spacing
        config = await promptForConfiguration();
        saveConfig(config);
        UI.info('Creating Claude Adapter API...');
      } else {
        UI.info('Using existing configuration');
      }

      // Step 3: Start the native proxy; it atomically selects an available port.
      const preferredPort = parseInt(options.port, 10) || 3080;
      const server = await startNativeServer(join(getConfigDir(), 'config.json'), preferredPort);
      const proxyUrl = server.url;
      UI.statusDone(true, `Claude Adapter running at ${UI.newUrl(proxyUrl)}`);

      // Step 4: Update Claude Code settings (if enabled)
      if (options.claudeSettings) {
        updateClaudeSettings(proxyUrl, config.models);
        UI.statusDone(true, 'Models configured:');

        // Display configured models
        UI.table([
          { label: 'Opus', value: config.models.opus },
          { label: 'Sonnet', value: config.models.sonnet },
          { label: 'Haiku', value: config.models.haiku },
        ]);
      } else {
        UI.info('Claude Code settings not updated (use manual configuration)');
        UI.hint(`Set ANTHROPIC_BASE_URL=${proxyUrl} in your Claude Code settings`);
      }

      UI.success('Claude Adapter is ready!');
      UI.info('Open a new terminal tab and run Claude Code.');
      UI.hint('Press Ctrl+C to stop the proxy server.');

      // Non-blocking update check
      checkForUpdates().then((update) => {
        if (update?.hasUpdate) {
          UI.updateNotify(update.current, update.latest);
        }
        UI.log('');
      });

      let shuttingDown = false;
      const shutdown = async (signal: NodeJS.Signals) => {
        if (shuttingDown) return;
        shuttingDown = true;
        UI.log('');
        await server.stop(signal);
        UI.success('Claude Adapter stopped');
        process.exit(0);
      };
      process.once('SIGINT', () => void shutdown('SIGINT'));
      process.once('SIGTERM', () => void shutdown('SIGTERM'));
      server.exited.then(({ code, signal }) => {
        if (shuttingDown) return;
        UI.error(`Native proxy stopped unexpectedly (code=${code}, signal=${signal})`);
        process.exit(code ?? 1);
      });
    } catch (error) {
      UI.statusDone(false, 'An error occurred');
      UI.error('Setup failed', error as Error);
      process.exit(1);
    }
  });

/**
 * Prompt user for configuration
 */
async function promptForConfiguration(): Promise<AdapterConfig> {
  const prefix = UI.dim('?');

  // Required configuration prompts
  const requiredAnswers = await inquirer.prompt([
    {
      type: 'input',
      name: 'baseUrl',
      prefix,
      message: 'OpenAI-compatible base URL:',
      default: 'https://api.openai.com/v1',
      transformer: (input: string) => UI.highlight(input),
      validate: (input: string) => {
        try {
          new URL(input);
          return true;
        } catch {
          return 'Please enter a valid URL';
        }
      },
    },
    {
      type: 'list',
      name: 'apiKeySource',
      prefix,
      message: 'API Key source:',
      choices: [
        { name: 'Enter API key', value: 'plaintext' },
        { name: 'Read from environment variable', value: 'environment' },
      ],
      default: 'plaintext',
    },
    {
      type: 'password',
      name: 'apiKey',
      prefix,
      message: 'API Key:',
      mask: '*',
      when: (answers) => answers.apiKeySource === 'plaintext',
      transformer: (input: string) => UI.highlight('*'.repeat(input.length)),
      validate: (input: string) => {
        if (!input || input.trim() === '') {
          return 'API key is required';
        }
        return true;
      },
    },
    {
      type: 'input',
      name: 'apiKeyEnv',
      prefix,
      message: 'API Key environment variable:',
      when: (answers) => answers.apiKeySource === 'environment',
      transformer: (input: string) => UI.highlight(input),
      validate: (input: string) => {
        try {
          parseApiKeyEnvInput(input);
          return true;
        } catch (error) {
          return (error as Error).message;
        }
      },
    },
    {
      type: 'input',
      name: 'opusModel',
      prefix,
      message: 'Alternative model for Opus:',
      transformer: (input: string) => UI.highlight(input),
      validate: (input: string) => {
        if (!input || input.trim() === '') {
          return 'Model name is required for Opus';
        }
        return true;
      },
    },
  ]);

  const opusModel = requiredAnswers.opusModel.trim();

  // Sonnet prompt
  const sonnetAnswer = await inquirer.prompt([
    {
      type: 'input',
      name: 'sonnetModel',
      prefix,
      message: 'Alternative model for Sonnet:',
      transformer: (input: string) => (input ? UI.highlight(input) : ''),
    },
  ]);

  const sonnetModel = sonnetAnswer.sonnetModel.trim() || opusModel;

  // If skipped, replace blank line with fallback display
  if (!sonnetAnswer.sonnetModel.trim()) {
    process.stdout.write('\x1b[1A\x1b[2K');
    console.log(`${prefix} Alternative model for Sonnet: ${UI.dim(`[${opusModel}]`)}`);
  }

  // Haiku prompt
  const haikuAnswer = await inquirer.prompt([
    {
      type: 'input',
      name: 'haikuModel',
      prefix,
      message: 'Alternative model for Haiku:',
      transformer: (input: string) => (input ? UI.highlight(input) : ''),
    },
  ]);

  const haikuModel = haikuAnswer.haikuModel.trim() || sonnetModel;

  // If skipped, replace blank line with fallback display
  if (!haikuAnswer.haikuModel.trim()) {
    process.stdout.write('\x1b[1A\x1b[2K');
    console.log(`${prefix} Alternative model for Haiku: ${UI.dim(`[${sonnetModel}]`)}`);
  }

  const upstreamHeadersAnswer = await inquirer.prompt([
    {
      type: 'input',
      name: 'upstreamHeaders',
      prefix,
      message: 'Upstream OpenAI headers JSON (optional):',
      transformer: (input: string) => (input ? UI.highlight(input) : ''),
      validate: (input: string) => {
        try {
          parseUpstreamHeadersInput(input);
          return true;
        } catch (error) {
          return (error as Error).message;
        }
      },
    },
  ]);

  const upstreamHeaders = parseUpstreamHeadersInput(upstreamHeadersAnswer.upstreamHeaders);
  const capabilityAnswer = await inquirer.prompt<{ assistantPrefill: AssistantPrefillMode }>([
    {
      type: 'list',
      name: 'assistantPrefill',
      prefix,
      message: 'Upstream assistant-prefill support:',
      choices: [
        {
          name: 'Unsupported (standard OpenAI-compatible API)',
          value: 'unsupported',
        },
        {
          name: 'continue_final_message extension (vLLM/llama.cpp)',
          value: 'continue_final_message',
        },
        { name: 'Native final-assistant continuation', value: 'native' },
      ],
      default: 'unsupported',
    },
  ]);
  const credentials =
    requiredAnswers.apiKeySource === 'environment'
      ? { apiKeyEnv: parseApiKeyEnvInput(requiredAnswers.apiKeyEnv) }
      : { apiKey: requiredAnswers.apiKey.trim() };

  return {
    baseUrl: requiredAnswers.baseUrl.trim(),
    ...credentials,
    models: {
      opus: opusModel,
      sonnet: sonnetModel,
      haiku: haikuModel,
    },
    upstreamHeaders,
    upstreamCapabilities: {
      assistantPrefill: capabilityAnswer.assistantPrefill,
    },
  };
}

// Run the CLI
program.parse();
