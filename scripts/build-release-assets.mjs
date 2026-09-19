#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import {
  chmodSync,
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
const packageJson = JSON.parse(readFileSync(join(rootDir, 'package.json'), 'utf8'));
const version = packageJson.version;
const platforms = [
  ['darwin-arm64', 'claude-adapter-native'],
  ['darwin-x64', 'claude-adapter-native'],
  ['linux-arm64-gnu', 'claude-adapter-native'],
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

function currentPlatform() {
  const suffix =
    process.platform === 'linux' ? '-gnu' : process.platform === 'win32' ? '-msvc' : '';
  const name = `${process.platform}-${process.arch}${suffix}`;
  const entry = platforms.find(([platform]) => platform === name);
  if (!entry)
    throw new Error(`Cannot package unsupported platform ${process.platform}-${process.arch}`);
  return entry;
}

function stageLocalBinary() {
  run('npm', ['run', 'build:native']);
  const [platform, binary] = currentPlatform();
  const source = join(rootDir, 'native', 'target', 'release', binary);
  const destination = join(rootDir, 'npm', platform, 'bin', binary);
  mkdirSync(dirname(destination), { recursive: true });
  cpSync(source, destination);
  if (process.platform !== 'win32') chmodSync(destination, 0o755);
}

function createMainPackage() {
  cpSync(join(rootDir, 'dist'), join(stagingDir, 'dist'), { recursive: true });
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
      'optionalDependencies',
      'engines',
    ].map((key) => [key, packageJson[key]])
  );
  stagedPackageJson.files = ['dist', 'LICENSE'];
  writeFileSync(
    join(stagingDir, 'package.json'),
    `${JSON.stringify(stagedPackageJson, null, 2)}\n`
  );
  run('npm', ['pack', '--pack-destination', releaseDir], { cwd: stagingDir });
  validateTarball(join(releaseDir, `${packageJson.name}-${version}.tgz`));
}

function createPlatformPackages() {
  let count = 0;
  for (const [platform, binary] of platforms) {
    const directory = join(rootDir, 'npm', platform);
    if (!existsSync(join(directory, 'bin', binary))) continue;
    run('npm', ['pack', '--pack-destination', releaseDir], { cwd: directory });
    const platformPackage = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
    validateTarball(join(releaseDir, `${platformPackage.name}-${version}.tgz`));
    count++;
  }
  if (process.env.REQUIRE_ALL_PLATFORMS === '1' && count !== platforms.length) {
    throw new Error(`Expected ${platforms.length} platform binaries, found ${count}`);
  }
}

function validateTarball(tarball) {
  if (!existsSync(tarball)) throw new Error(`Expected package tarball not found: ${tarball}`);
  const list = spawnSync('tar', ['-tzf', tarball], { encoding: 'utf8' });
  if (list.status !== 0) throw new Error(list.stderr);
  const forbidden = list.stdout
    .split('\n')
    .filter(Boolean)
    .filter((entry) => {
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
}

function validateCleanInstall() {
  if (process.env.CLEAN_INSTALL !== '1') return;
  const [platform] = currentPlatform();
  const platformPackage = JSON.parse(
    readFileSync(join(rootDir, 'npm', platform, 'package.json'), 'utf8')
  );
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
        '--ignore-scripts',
        '--no-audit',
        '--no-fund',
        join(releaseDir, `${platformPackage.name}-${version}.tgz`),
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
  for (const [platform] of platforms) {
    rmSync(join(rootDir, 'npm', platform, 'bin'), { recursive: true, force: true });
  }
}

resetBuildDirs();
try {
  if (process.env.RELEASE_BINARIES_STAGED !== '1') stageLocalBinary();
  run('npm', ['run', 'build:release']);
  createPlatformPackages();
  createMainPackage();
  validateCleanInstall();
  createSourceArchive();
  console.log(`Release assets written to ${releaseDir}`);
} finally {
  cleanStagedBinaries();
}
