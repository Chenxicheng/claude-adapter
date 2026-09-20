import { EventEmitter } from 'node:events';
import { ChildProcess, spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { PassThrough } from 'node:stream';
import { resolve } from 'node:path';
import { nativeTargetForPlatform, resolveNativeBinary, startNativeServer } from '../src/native';

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
const originalPlatform = process.platform;
const originalArch = process.arch;

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
  Object.defineProperty(process, 'platform', { value: originalPlatform, configurable: true });
  Object.defineProperty(process, 'arch', { value: originalArch, configurable: true });
});

describe('native platform selection', () => {
  it.each([
    ['linux', 'x64', 'linux-x64-gnu', 'claude-adapter-native'],
    ['win32', 'x64', 'win32-x64-msvc', 'claude-adapter-native.exe'],
  ] as const)('maps %s-%s to its bundled binary', (platform, arch, directory, binary) => {
    expect(nativeTargetForPlatform(platform, arch)).toEqual({ directory, binary });
  });

  it('rejects unsupported targets with an actionable message', () => {
    expect(() => nativeTargetForPlatform('darwin', 'arm64')).toThrow(
      'Supported platforms: Linux glibc x64 and Windows x64.'
    );
  });

  it('explains how to recover when the bundled binary is missing', () => {
    Object.defineProperty(process, 'platform', { value: 'win32', configurable: true });
    Object.defineProperty(process, 'arch', { value: 'x64', configurable: true });
    existsSyncMock.mockReturnValue(false);
    expect(() => resolveNativeBinary()).toThrow(
      /Offline package is missing bin\/win32-x64-msvc\/claude-adapter-native\.exe.*Reinstall claude-adapter@2\.0\.1/
    );
  });

  it('publishes one offline package without platform dependencies', () => {
    const manifest = JSON.parse(readFileSync(resolve('package.json'), 'utf8'));
    expect(manifest.version).toBe('2.0.1');
    expect(manifest.files).toEqual(['dist', 'bin', 'LICENSE']);
    expect(manifest.optionalDependencies).toBeUndefined();
    expect(manifest.bundleDependencies).toEqual(['chalk', 'commander', 'inquirer']);
  });
});

describe('native process lifecycle', () => {
  beforeEach(() => {
    Object.defineProperty(process, 'platform', { value: 'win32', configurable: true });
    Object.defineProperty(process, 'arch', { value: 'x64', configurable: true });
  });

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
