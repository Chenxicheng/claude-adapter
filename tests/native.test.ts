import { EventEmitter } from 'node:events';
import { ChildProcess, spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { PassThrough } from 'node:stream';
import { resolve } from 'node:path';
import { nativePackageForPlatform, resolveNativeBinary, startNativeServer } from '../src/native';

jest.mock('node:child_process', () => ({
  ...jest.requireActual('node:child_process'),
  spawn: jest.fn(),
}));
jest.mock('node:fs', () => ({
  ...jest.requireActual('node:fs'),
  existsSync: jest.fn(),
}));

const spawnMock = jest.mocked(spawn);
const existsSyncMock = jest.mocked(existsSync);

interface FakeChild extends ChildProcess {
  stdout: PassThrough;
  stderr: PassThrough;
  stdin: PassThrough;
  finish(code: number | null, signal: NodeJS.Signals | null): void;
}

function createFakeChild(): FakeChild {
  const child = Object.assign(new EventEmitter(), {
    stdout: new PassThrough(),
    stderr: new PassThrough(),
    stdin: new PassThrough(),
    exitCode: null,
    signalCode: null,
  }) as unknown as FakeChild;
  child.finish = (code, signal) => {
    Object.defineProperty(child, 'exitCode', { value: code, writable: true });
    Object.defineProperty(child, 'signalCode', { value: signal, writable: true });
    child.emit('exit', code, signal);
  };
  child.kill = jest.fn((signal: NodeJS.Signals = 'SIGTERM') => {
    child.finish(null, signal);
    return true;
  });
  const end = child.stdin.end.bind(child.stdin);
  child.stdin.end = ((...args: Parameters<typeof child.stdin.end>) => {
    const result = end(...args);
    child.finish(0, null);
    return result;
  }) as typeof child.stdin.end;
  return child;
}

beforeEach(() => {
  spawnMock.mockReset();
  existsSyncMock.mockReset().mockReturnValue(true);
});

afterEach(() => {
  jest.useRealTimers();
});

describe('native platform selection', () => {
  it.each([
    ['darwin', 'arm64', 'claude-adapter-darwin-arm64'],
    ['darwin', 'x64', 'claude-adapter-darwin-x64'],
    ['linux', 'arm64', 'claude-adapter-linux-arm64-gnu'],
    ['linux', 'x64', 'claude-adapter-linux-x64-gnu'],
    ['win32', 'x64', 'claude-adapter-win32-x64-msvc'],
  ] as const)('maps %s-%s to its package', (platform, arch, expected) => {
    expect(nativePackageForPlatform(platform, arch)).toBe(expected);
  });

  it('rejects unsupported targets with an actionable message', () => {
    expect(() => nativePackageForPlatform('win32', 'arm64')).toThrow(
      'Supported platforms: macOS arm64/x64, Linux glibc arm64/x64, and Windows x64.'
    );
  });

  it('explains how to recover when the platform package is missing', () => {
    existsSyncMock.mockReturnValue(false);
    expect(() => resolveNativeBinary()).toThrow(
      /Reinstall with "npm install -g claude-adapter@2\.0\.0".*Supported platforms:/
    );
  });

  it.each([
    ['darwin-arm64', 'darwin', 'arm64', 'claude-adapter-native'],
    ['darwin-x64', 'darwin', 'x64', 'claude-adapter-native'],
    ['linux-arm64-gnu', 'linux', 'arm64', 'claude-adapter-native'],
    ['linux-x64-gnu', 'linux', 'x64', 'claude-adapter-native'],
    ['win32-x64-msvc', 'win32', 'x64', 'claude-adapter-native.exe'],
  ])('publishes correct metadata for %s', (directory, os, cpu, binary) => {
    const manifest = JSON.parse(readFileSync(resolve('npm', directory, 'package.json'), 'utf8'));
    expect(manifest.version).toBe('2.0.0');
    expect(manifest.os).toEqual([os]);
    expect(manifest.cpu).toEqual([cpu]);
    expect(manifest.files).toEqual([`bin/${binary}`]);
  });
});

describe('native process lifecycle', () => {
  it('accepts one ready record and clears the shutdown timeout after a fast exit', async () => {
    jest.useFakeTimers();
    const child = createFakeChild();
    spawnMock.mockReturnValue(child);

    const starting = startNativeServer('/tmp/config.json', 3080);
    child.stdout.write('CLAUDE_ADAPTER_READY={"url":"http://localhost:3080"}\n');
    const server = await starting;
    await server.stop();

    expect(server.url).toBe('http://localhost:3080');
    expect(jest.getTimerCount()).toBe(0);
    expect(child.kill).not.toHaveBeenCalledWith('SIGKILL');
    const options = spawnMock.mock.calls[0][2]!;
    if (process.platform === 'win32') {
      expect(options.stdio?.[0]).toBe('pipe');
      expect(options.env?.CLAUDE_ADAPTER_STDIN_SHUTDOWN).toBe('1');
    } else {
      expect(options.stdio?.[0]).toBe('ignore');
      expect(options.env?.CLAUDE_ADAPTER_STDIN_SHUTDOWN).toBeUndefined();
      expect(child.kill).toHaveBeenCalledWith('SIGTERM');
    }
  });

  it('rejects an invalid ready record', async () => {
    const child = createFakeChild();
    spawnMock.mockReturnValue(child);
    const starting = startNativeServer('/tmp/config.json', 3080);
    child.stdout.write('CLAUDE_ADAPTER_READY={}\n');
    await expect(starting).rejects.toThrow('ready record has no URL');
  });

  it('rejects when the native process exits before ready', async () => {
    const child = createFakeChild();
    spawnMock.mockReturnValue(child);
    const starting = startNativeServer('/tmp/config.json', 3080);
    child.finish(1, null);
    await expect(starting).rejects.toThrow('Native proxy exited before startup');
  });

  it('terminates a native process that exceeds the startup timeout', async () => {
    jest.useFakeTimers();
    const child = createFakeChild();
    spawnMock.mockReturnValue(child);
    const starting = startNativeServer('/tmp/config.json', 3080);
    const rejected = expect(starting).rejects.toThrow('did not start within 10 seconds');
    await jest.advanceTimersByTimeAsync(10_000);
    await rejected;
    expect(jest.getTimerCount()).toBe(0);
  });
});
