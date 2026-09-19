import { ChildProcess, spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { version } from '../package.json';

const STARTUP_TIMEOUT_MS = 10_000;
const SHUTDOWN_TIMEOUT_MS = 10_000;
const READY_PREFIX = 'CLAUDE_ADAPTER_READY=';

const PLATFORM_PACKAGES: Record<string, string> = {
  'darwin-arm64': 'claude-adapter-darwin-arm64',
  'darwin-x64': 'claude-adapter-darwin-x64',
  'linux-arm64': 'claude-adapter-linux-arm64-gnu',
  'linux-x64': 'claude-adapter-linux-x64-gnu',
  'win32-x64': 'claude-adapter-win32-x64-msvc',
};

export interface NativeServer {
  url: string;
  exited: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  stop(signal?: NodeJS.Signals): Promise<void>;
}

export function nativePackageForPlatform(
  platform: NodeJS.Platform = process.platform,
  arch: string = process.arch
): string {
  const packageName = PLATFORM_PACKAGES[`${platform}-${arch}`];
  if (!packageName) {
    throw new Error(
      `Unsupported platform ${platform}-${arch}. Supported platforms: macOS arm64/x64, Linux glibc arm64/x64, and Windows x64.`
    );
  }
  if (platform === 'linux' && !hasGlibc()) {
    throw new Error('Linux musl is not supported yet; use a glibc-based distribution.');
  }
  return packageName;
}

export function resolveNativeBinary(): string {
  const binaryName =
    process.platform === 'win32' ? 'claude-adapter-native.exe' : 'claude-adapter-native';
  const packageName = nativePackageForPlatform();
  try {
    return require.resolve(`${packageName}/bin/${binaryName}`);
  } catch {
    const developmentBinary = resolve(__dirname, '..', 'native', 'target', 'release', binaryName);
    if (existsSync(developmentBinary)) {
      return developmentBinary;
    }
    throw new Error(
      `Native package ${packageName} is missing for ${process.platform}-${process.arch}. ` +
        `Reinstall with "npm install -g claude-adapter@${version}". ` +
        'Supported platforms: macOS arm64/x64, Linux glibc arm64/x64, and Windows x64.'
    );
  }
}

export async function startNativeServer(configPath: string, port: number): Promise<NativeServer> {
  const binary = resolveNativeBinary();
  const windowsStdinShutdown = process.platform === 'win32';
  const env = { ...process.env };
  if (windowsStdinShutdown) env.CLAUDE_ADAPTER_STDIN_SHUTDOWN = '1';
  else delete env.CLAUDE_ADAPTER_STDIN_SHUTDOWN;
  const child = spawn(binary, ['--config', configPath, '--port', String(port)], {
    stdio: [windowsStdinShutdown ? 'pipe' : 'ignore', 'pipe', 'pipe'],
    windowsHide: true,
    env,
  });
  child.stderr?.pipe(process.stderr);

  const exited = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>(
    (resolveExit) => {
      child.once('exit', (code, signal) => resolveExit({ code, signal }));
      child.once('error', () => resolveExit({ code: null, signal: null }));
    }
  );
  let url: string;
  try {
    url = await waitForReady(child, exited);
  } catch (error) {
    await stopChild(child, exited, 'SIGTERM');
    throw error;
  }

  return {
    url,
    exited,
    stop: (signal: NodeJS.Signals = 'SIGTERM') => stopChild(child, exited, signal),
  };
}

function waitForReady(child: ChildProcess, exited: NativeServer['exited']): Promise<string> {
  return new Promise((resolveReady, reject) => {
    let output = '';
    let settled = false;
    const timeout = setTimeout(
      () => finish(new Error('Native proxy did not start within 10 seconds.')),
      STARTUP_TIMEOUT_MS
    );

    const finish = (error?: Error, url?: string) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (error) reject(error);
      else resolveReady(url!);
    };

    child.once('error', (error) =>
      finish(new Error(`Failed to start native proxy: ${error.message}`))
    );
    child.stdout?.on('data', (chunk) => {
      output += chunk.toString();
      let newline = output.indexOf('\n');
      while (newline >= 0) {
        const line = output.slice(0, newline).trim();
        output = output.slice(newline + 1);
        if (line.startsWith(READY_PREFIX)) {
          try {
            const ready = JSON.parse(line.slice(READY_PREFIX.length)) as { url?: string };
            if (!ready.url) throw new Error('ready record has no URL');
            finish(undefined, ready.url);
          } catch (error) {
            finish(new Error(`Invalid native ready record: ${(error as Error).message}`));
          }
        } else if (line) {
          process.stdout.write(`${line}\n`);
        }
        newline = output.indexOf('\n');
      }
    });
    exited.then(({ code, signal }) => {
      finish(new Error(`Native proxy exited before startup (code=${code}, signal=${signal}).`));
    });
  });
}

async function stopChild(
  child: ChildProcess,
  exited: NativeServer['exited'],
  signal: NodeJS.Signals
): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') child.stdin?.end('shutdown\n');
  else child.kill(signal);
  const timedOut = await new Promise<boolean>((resolveTimeout) => {
    const timeout = setTimeout(() => resolveTimeout(true), SHUTDOWN_TIMEOUT_MS);
    exited.then(() => {
      clearTimeout(timeout);
      resolveTimeout(false);
    });
  });
  if (timedOut && child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await exited;
  }
}

function hasGlibc(): boolean {
  if (process.platform !== 'linux') return true;
  const report = process.report?.getReport() as { header?: { glibcVersionRuntime?: string } };
  return Boolean(report.header?.glibcVersionRuntime);
}
