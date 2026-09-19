#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const releaseDir = join(rootDir, 'release');
const stagingDir = join(releaseDir, 'pack-staging');
const binaryDir = join(rootDir, 'bin');
const packageJson = JSON.parse(readFileSync(join(rootDir, 'package.json'), 'utf8'));
const version = packageJson.version;
const platforms = [
  ['linux-x64-gnu', 'claude-adapter-native'],
  ['win32-x64-msvc', 'claude-adapter-native.exe'],
];

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: rootDir,
    stdio: 'inherit',
    shell: process.platform === 'win32',
    ...options,
  });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} failed with exit code ${result.status ?? 1}`);
  }
}

function resetBuildDirs() {
  rmSync(releaseDir, { recursive: true, force: true });
  rmSync(join(rootDir, 'dist'), { recursive: true, force: true });
  mkdirSync(stagingDir, { recursive: true });
}

function validateStagedBinaries() {
  for (const [platform, binary] of platforms) {
    if (!existsSync(join(binaryDir, platform, binary))) {
      throw new Error(`Missing offline binary bin/${platform}/${binary}`);
    }
  }
}

function createMainPackage() {
  cpSync(join(rootDir, 'dist'), join(stagingDir, 'dist'), { recursive: true });
  cpSync(binaryDir, join(stagingDir, 'bin'), { recursive: true });
  cpSync(join(rootDir, 'LICENSE'), join(stagingDir, 'LICENSE'));
  const stagedPackageJson = Object.fromEntries(
    [
      'name',
      'version',
      'description',
      'bin',
      'keywords',
      'author',
      'license',
      'repository',
      'bugs',
      'homepage',
      'dependencies',
      'bundleDependencies',
      'engines',
    ].map((key) => [key, packageJson[key]])
  );
  stagedPackageJson.files = ['dist', 'bin', 'LICENSE'];
  writeFileSync(
    join(stagingDir, 'package.json'),
    `${JSON.stringify(stagedPackageJson, null, 2)}\n`
  );
  run('npm', ['install', '--omit=dev', '--ignore-scripts', '--no-audit', '--no-fund'], {
    cwd: stagingDir,
  });
  run('npm', ['pack', '--pack-destination', releaseDir], { cwd: stagingDir });
  validateTarball(join(releaseDir, `${packageJson.name}-${version}.tgz`), true);
}

function validateTarball(tarball, requireBinaries = false) {
  if (!existsSync(tarball)) throw new Error(`Expected package tarball not found: ${tarball}`);
  const list = spawnSync('tar', ['-tzf', tarball], { encoding: 'utf8' });
  if (list.status !== 0) throw new Error(list.stderr);
  const forbidden = list.stdout
    .split('\n')
    .filter(Boolean)
    .filter((entry) => {
      if (entry.startsWith('package/node_modules/')) return false;
      const name = basename(entry).toLowerCase();
      return (
        name === 'readme' ||
        name.startsWith('readme.') ||
        name.endsWith('.md') ||
        entry.includes('/test/') ||
        entry.includes('/tests/') ||
        name.includes('.test.')
      );
    });
  if (forbidden.length)
    throw new Error(`Package contains forbidden files:\n${forbidden.join('\n')}`);
  if (requireBinaries) {
    for (const [platform, binary] of platforms) {
      const expected = `package/bin/${platform}/${binary}`;
      if (!list.stdout.split('\n').includes(expected)) {
        throw new Error(`Package is missing ${expected}`);
      }
    }
    for (const dependency of packageJson.bundleDependencies) {
      const expected = `package/node_modules/${dependency}/package.json`;
      if (!list.stdout.split('\n').includes(expected)) {
        throw new Error(`Package is missing bundled dependency ${dependency}`);
      }
    }
  }
}

function validateCleanInstall() {
  if (process.env.CLEAN_INSTALL !== '1') return;
  if (process.platform !== 'linux' || process.arch !== 'x64') {
    throw new Error('Clean install validation requires Linux x64');
  }
  const directory = mkdtempSync(join(tmpdir(), 'claude-adapter-install-'));
  try {
    writeFileSync(join(directory, 'package.json'), '{"private":true}\n');
    const configPath = join(directory, 'config.json');
    writeFileSync(
      configPath,
      JSON.stringify({
        baseUrl: 'http://127.0.0.1:1/v1',
        apiKey: 'clean-install',
        models: { opus: 'mock', sonnet: 'mock', haiku: 'mock' },
      })
    );
    run(
      'npm',
      [
        'install',
        '--offline',
        '--ignore-scripts',
        '--no-audit',
        '--no-fund',
        '--cache',
        join(directory, 'npm-cache'),
        join(releaseDir, `${packageJson.name}-${version}.tgz`),
      ],
      { cwd: directory }
    );
    run(
      'node',
      [join(directory, 'node_modules', packageJson.name, 'dist', 'cli.js'), '--version'],
      {
        cwd: directory,
      }
    );
    run(
      'node',
      [
        '-e',
        `const native=require(${JSON.stringify(join(directory, 'node_modules', packageJson.name, 'dist', 'native.js'))});` +
          `const {spawnSync}=require('node:child_process');` +
          `(async()=>{` +
          `const server=await native.startNativeServer(${JSON.stringify(configPath)},0);` +
          `const stopStarted=Date.now();` +
          `await server.stop();` +
          `if(Date.now()-stopStarted>=2000)throw new Error('native graceful shutdown exceeded 2 seconds');` +
          `const failed=spawnSync(native.resolveNativeBinary(),['--config',${JSON.stringify(join(directory, 'missing.json'))},'--port','0']);` +
          `if(failed.status===0)throw new Error('native startup failure was not reported');` +
          `})().catch(error=>{console.error(error);process.exit(1)})`,
      ],
      { cwd: directory }
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

function createSourceArchive() {
  run('git', [
    'archive',
    '--format=zip',
    '--output',
    join(releaseDir, `${packageJson.name}-v${version}-source.zip`),
    'HEAD',
  ]);
}

function cleanStagedBinaries() {
  rmSync(binaryDir, { recursive: true, force: true });
}

resetBuildDirs();
try {
  validateStagedBinaries();
  run('npm', ['run', 'build:release']);
  createMainPackage();
  validateCleanInstall();
  createSourceArchive();
  console.log(`Release assets written to ${releaseDir}`);
} finally {
  cleanStagedBinaries();
}
